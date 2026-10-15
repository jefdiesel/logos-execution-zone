use crate::primitives::types::{
    FfiAccountId, FfiNonce, FfiVec,
    transaction::{
        FfiPrivateAction, FfiPublicAction, FfiPublicEffect, FfiSignaturePubKeyEntry, FfiTransaction,
    },
};

use crate::api::types::{FfiVec, transaction::FfiTransaction};

pub type FfiVecU8 = FfiVec<u8>;

pub type FfiBlockBody = FfiVec<FfiTransaction>;

pub type FfiProof = FfiVecU8;

pub type FfiProgramDeploymentMessage = FfiVecU8;
