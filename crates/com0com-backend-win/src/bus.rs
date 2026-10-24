//! Typed client for the private `IOCTL_COM0COM_*` protocol.

use std::mem::size_of;
use std::sync::Arc;

use com0com_common::ioctls;
use com0com_common::protocol::{
    Com0comPairParams, CreatePairRequest, DeletePairRequest, EnumPairEntry, EnumPairsResponse,
    GetParamsRequest, SetParamsRequest,
};
use com0com_core::{Pair, PairId, PairParams, SdkError, MAX_PAIRS, MAX_PAIR_ID};

use crate::convert::{params_to_wire, wire_to_params};
use crate::transport::{struct_bytes, struct_of, BusTransport};

pub(crate) struct BusClient {
    transport: Arc<dyn BusTransport>,
}

impl BusClient {
    pub(crate) fn new(transport: Arc<dyn BusTransport>) -> Self {
        Self { transport }
    }

    pub(crate) fn transport(&self) -> Arc<dyn BusTransport> {
        self.transport.clone()
    }

    fn call(
        &self,
        op: &'static str,
        code: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, SdkError> {
        match self.transport.ioctl(op, code, input, output) {
            Ok(bytes) => Ok(bytes),
            // The driver reports a missing pair as STATUS_INVALID_PARAMETER.
            // It cannot also mean "bad parameters" in create, because those are
            // validated before the IOCTL is sent.
            Err(SdkError::Win32 { code: 87, .. }) => {
                if op == "CREATE_PAIR" {
                    Err(SdkError::InvalidArgument(
                        "driver rejected the pair parameters".into(),
                    ))
                } else {
                    Err(SdkError::NotFound)
                }
            }
            Err(e) => Err(e),
        }
    }

    fn enum_pairs(&self) -> Result<Vec<Pair>, SdkError> {
        let mut out = vec![0u8; size_of::<EnumPairsResponse>()];
        let bytes = self.call(
            "ENUM_PAIRS",
            ioctls::IOCTL_COM0COM_ENUM_PAIRS,
            &[],
            &mut out,
        )?;
        if bytes < size_of::<u32>() {
            return Err(SdkError::Internal(format!(
                "ENUM_PAIRS returned {bytes} bytes, expected at least 4"
            )));
        }
        let resp: EnumPairsResponse = struct_of(&out);
        let count = resp.count as usize;
        if count > MAX_PAIRS {
            return Err(SdkError::Internal(format!(
                "ENUM_PAIRS returned {count} pairs, limit is {MAX_PAIRS}"
            )));
        }
        let mut pairs = Vec::with_capacity(count);
        for entry in resp.entries.iter().take(count) {
            pairs.push(entry_to_pair(entry)?);
        }
        Ok(pairs)
    }

    pub(crate) fn list_pairs(&self) -> Result<Vec<Pair>, SdkError> {
        self.enum_pairs()
    }

    pub(crate) fn create_pair(
        &self,
        id: Option<PairId>,
        params: &PairParams,
    ) -> Result<Pair, SdkError> {
        params.validate()?;
        let wire = params_to_wire(params)?;
        let id = match id {
            Some(id) => {
                check_id(id)?;
                self.send_create(id, &wire)?;
                id
            }
            None => {
                // Retry because another process can take the candidate id
                // between ENUM_PAIRS and CREATE_PAIR.
                let mut created = None;
                for _ in 0..MAX_PAIRS {
                    let used = self.enum_pairs()?;
                    let Some(candidate) =
                        (0..=MAX_PAIR_ID).find(|id| !used.iter().any(|p| p.id == *id))
                    else {
                        return Err(SdkError::Busy);
                    };
                    match self.send_create(candidate, &wire) {
                        Ok(()) => {
                            created = Some(candidate);
                            break;
                        }
                        Err(SdkError::AlreadyExists) => continue,
                        Err(e) => return Err(e),
                    }
                }
                created.ok_or(SdkError::Busy)?
            }
        };
        Ok(Pair {
            id,
            params: params.clone(),
        })
    }

    fn send_create(&self, id: PairId, wire: &Com0comPairParams) -> Result<(), SdkError> {
        let req = CreatePairRequest {
            pair_id: id as u32,
            params: *wire,
        };
        self.call(
            "CREATE_PAIR",
            ioctls::IOCTL_COM0COM_CREATE_PAIR,
            struct_bytes(&req),
            &mut [],
        )?;
        Ok(())
    }

    pub(crate) fn delete_pair(&self, id: PairId) -> Result<(), SdkError> {
        check_id(id)?;
        let req = DeletePairRequest { pair_id: id as u32 };
        self.call(
            "DELETE_PAIR",
            ioctls::IOCTL_COM0COM_DELETE_PAIR,
            struct_bytes(&req),
            &mut [],
        )?;
        Ok(())
    }

    pub(crate) fn get_params(&self, id: PairId) -> Result<PairParams, SdkError> {
        check_id(id)?;
        let req = GetParamsRequest {
            pair_id: id as u32,
            port_index: 0,
        };
        let mut out = [0u8; size_of::<Com0comPairParams>()];
        let bytes = self.call(
            "GET_PARAMS",
            ioctls::IOCTL_COM0COM_GET_PARAMS,
            struct_bytes(&req),
            &mut out,
        )?;
        if bytes < out.len() {
            return Err(SdkError::Internal(format!(
                "GET_PARAMS returned {bytes} bytes, expected {}",
                out.len()
            )));
        }
        let wire: Com0comPairParams = struct_of(&out);
        wire_to_params(&wire)
    }

    pub(crate) fn set_params(&self, id: PairId, params: &PairParams) -> Result<(), SdkError> {
        check_id(id)?;
        params.validate()?;
        let wire = params_to_wire(params)?;
        let req = SetParamsRequest {
            pair_id: id as u32,
            port_index: 0,
            params: wire,
        };
        self.call(
            "SET_PARAMS",
            ioctls::IOCTL_COM0COM_SET_PARAMS,
            struct_bytes(&req),
            &mut [],
        )?;
        Ok(())
    }
}

fn check_id(id: PairId) -> Result<(), SdkError> {
    if id > MAX_PAIR_ID {
        return Err(SdkError::InvalidArgument(format!(
            "pair id {id} is outside 0..={MAX_PAIR_ID}"
        )));
    }
    Ok(())
}

fn entry_to_pair(entry: &EnumPairEntry) -> Result<Pair, SdkError> {
    let id = u8::try_from(entry.pair_id)
        .ok()
        .filter(|id| *id <= MAX_PAIR_ID)
        .ok_or_else(|| {
            SdkError::Internal(format!("driver returned invalid pair id {}", entry.pair_id))
        })?;
    Ok(Pair {
        id,
        params: wire_to_params(&entry.params)?,
    })
}
