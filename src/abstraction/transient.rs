// Copyright (c) 2019-2020, Arm Limited, All Rights Reserved
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may
// not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//          http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
// WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Module for abstracting resource handle management
//!
//! This module presents an abstraction over the TPM functionality exposed through the core
//! `Context` structure. The abstraction works by hiding resource handle management from the
//! client. This is achieved by passing objects back and forth in the form of contexts. Thus, when
//! an object is created, its saved context is returned and the object is flushed from the TPM.
//! Whenever the client needs to use said object, it calls the desired operation with the context
//! as a parameter - the context is loaded in the TPM, the operation performed and the context
//! flushed out again before the result is returned.
//!
//! Object contexts thus act as an opaque handle that can, however, be used by the client to seralize
//! and persist the underlying data.
use crate::constants::*;
use crate::response_code::{Error, Result, WrapperErrorKind as ErrorKind};
use crate::tss2_esys::*;
use crate::utils::{self, get_rsa_public, PublicIdUnion, TpmsContext, TpmtTkVerified};
use crate::{Context, Tcti, NO_SESSIONS};
use log::error;
use std::convert::{TryFrom, TryInto};

/// Structure offering an abstracted programming experience.
///
/// The `TransientObjectContext` makes use of a root key from which the other, client-controlled
/// keyes are derived.
///
/// Currently, only functionality necessary for RSA key creation and usage (for signing and
/// verifying signatures) is implemented.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug)]
pub struct TransientObjectContext {
    context: Context,
    root_key_handle: ESYS_TR,
}

impl TransientObjectContext {
    /// Create a new `TransientObjectContext`.
    ///
    /// The root key is created as a primary key in the Owner hierarchy and thus authentication is
    /// needed for the hierarchy. The authentication value is generated by the TPM itself, with a
    /// length provided as a parameter, and never exposed outside the context.
    ///
    /// # Safety
    /// * it is the responsibility of the client to ensure that the context can be initialized
    /// safely, threading-wise
    ///
    /// # Constraints
    /// * `root_key_size` must be 1024 or 2048
    /// * `root_key_auth_size` must be at most 32
    ///
    /// # Errors
    /// * errors are returned if any method calls return an error: `Context::get_random`,
    /// `Context::start_auth_session`, `Context::create_primary_key`, `Context::flush_context`,
    /// `Context::set_handle_auth`
    /// * if the root key authentication size is given greater than 32 or if the root key size is
    /// not 1024 or 2048, a `WrongParamSize` wrapper error is returned
    pub unsafe fn new(
        tcti: Tcti,
        root_key_size: usize,
        root_key_auth_size: usize,
        owner_hierarchy_auth: &[u8],
    ) -> Result<Self> {
        if root_key_auth_size > 32 {
            return Err(Error::local_error(ErrorKind::WrongParamSize));
        }
        if root_key_size != 1024 && root_key_size != 2048 {
            error!("The reference implementation only supports key sizes of 1,024 and 2,048 bits.");
            return Err(Error::local_error(ErrorKind::WrongParamSize));
        }
        let mut context = Context::new(tcti)?;
        let root_key_auth: Vec<u8> = if root_key_auth_size > 0 {
            context.get_random(root_key_auth_size)?
        } else {
            vec![]
        };
        if !owner_hierarchy_auth.is_empty() {
            context.set_handle_auth(ESYS_TR_RH_OWNER, owner_hierarchy_auth)?;
        }

        let root_key_handle = context.create_primary_key(
            ESYS_TR_RH_OWNER,
            &get_rsa_public(true, true, false, root_key_size.try_into().unwrap()), // should not fail on supported targets, given the checks above
            &root_key_auth,
            &[],
            &[],
            &[],
        )?;

        let new_session = context.start_auth_session(
            NO_SESSIONS,
            root_key_handle,
            ESYS_TR_NONE,
            &[],
            TPM2_SE_HMAC,
            utils::TpmtSymDefBuilder::aes_256_cfb(),
            TPM2_ALG_SHA256,
        )?;
        let (old_session, _, _) = context.sessions();
        context.set_sessions((new_session, ESYS_TR_NONE, ESYS_TR_NONE));
        context.flush_context(old_session)?;
        Ok(TransientObjectContext {
            context,
            root_key_handle,
        })
    }

    /// Create a new RSA signing key.
    ///
    /// The key is created with most parameters defaulted as described for the `get_rsa_public`
    /// method. The authentication value is generated by the TPM and returned along with the key
    /// context.
    ///
    /// # Constraints
    /// * `key_size` must be 1024 or 2048
    /// * `auth_size` must be at most 32
    ///
    /// # Errors
    /// * if the authentication size is given larger than 32 or if the requested key size is not
    /// 1024 or 2048, a `WrongParamSize` wrapper error is returned
    /// * errors are returned if any method calls return an error: `Context::get_random`,
    /// `TransientObjectContext::set_session_attrs`, `Context::create_key`, `Context::load`,
    /// `Context::context_save`, `Context::context_flush`
    pub fn create_rsa_signing_key(
        &mut self,
        key_size: usize,
        auth_size: usize,
    ) -> Result<(TpmsContext, Vec<u8>)> {
        if auth_size > 32 {
            return Err(Error::local_error(ErrorKind::WrongParamSize));
        }
        if key_size != 1024 && key_size != 2048 {
            return Err(Error::local_error(ErrorKind::WrongParamSize));
        }
        let key_auth = if auth_size > 0 {
            self.set_session_attrs()?;
            self.context.get_random(auth_size)?
        } else {
            vec![]
        };
        self.set_session_attrs()?;
        let (key_priv, key_pub) = self.context.create_key(
            self.root_key_handle,
            &get_rsa_public(false, false, true, key_size.try_into().unwrap()), // should not fail on valid targets, given the checks above
            &key_auth,
            &[],
            &[],
            &[],
        )?;
        self.set_session_attrs()?;
        let key_handle = self.context.load(self.root_key_handle, key_priv, key_pub)?;

        self.set_session_attrs()?;
        let key_context = self.context.context_save(key_handle).or_else(|e| {
            self.context.flush_context(key_handle)?;
            Err(e)
        })?;
        self.context.flush_context(key_handle)?;
        Ok((key_context, key_auth))
    }

    /// Load a previously generated RSA public key.
    ///
    /// Returns the key context.
    ///
    /// # Constraints
    /// * `public_key` must be 128 or 256 elements long
    ///
    /// # Errors
    /// * if the public key length is different than 1024 or 2048 bits, a `WrongParamSize` wrapper error is returned
    /// * errors are returned if any method calls return an error:
    /// `TransientObjectContext::`set_session_attrs`, `Context::load_external_public`,
    /// `Context::context_save`, `Context::flush_context`
    pub fn load_external_rsa_public_key(&mut self, public_key: &[u8]) -> Result<TpmsContext> {
        if public_key.len() != 128 && public_key.len() != 256 {
            return Err(Error::local_error(ErrorKind::WrongParamSize));
        }
        let mut pk_buffer = [0_u8; 512];
        pk_buffer[..public_key.len()].clone_from_slice(&public_key[..public_key.len()]);

        let pk = TPMU_PUBLIC_ID {
            rsa: TPM2B_PUBLIC_KEY_RSA {
                size: public_key.len().try_into().unwrap(), // should not fail on valid targets, given the checks above
                buffer: pk_buffer,
            },
        };

        let mut public = get_rsa_public(
            false,
            false,
            true,
            u16::try_from(public_key.len()).unwrap() * 8_u16, // should not fail on valid targets, given the checks above
        );
        public.publicArea.unique = pk;

        self.set_session_attrs()?;
        let key_handle = self.context.load_external_public(&public, TPM2_RH_OWNER)?;

        self.set_session_attrs()?;
        let key_context = self.context.context_save(key_handle).or_else(|e| {
            self.context.flush_context(key_handle)?;
            Err(e)
        })?;
        self.context.flush_context(key_handle)?;

        Ok(key_context)
    }

    /// Read the public part from a previously generated key.
    ///
    /// The method takes the key as a parameter and returns its public part.
    ///
    /// # Errors
    /// * errors are returned if any method calls return an error: `Context::context_load`,
    /// `Context::read_public`, `Context::flush_context`,
    /// `TransientObjectContext::set_session_attrs`
    pub fn read_public_key(&mut self, key_context: TpmsContext) -> Result<Vec<u8>> {
        self.set_session_attrs()?;
        let key_handle = self.context.context_load(key_context)?;

        self.set_session_attrs()?;
        let key_pub_id = self.context.read_public(key_handle).or_else(|e| {
            self.context.flush_context(key_handle)?;
            Err(e)
        })?;
        let key = match unsafe { PublicIdUnion::from_public(&key_pub_id)? } {
            // call should be safe given our trust in the TSS library
            PublicIdUnion::Rsa(pub_key) => {
                let mut key = pub_key.buffer.to_vec();
                key.truncate(pub_key.size.try_into().unwrap()); // should not fail on supported targets
                key
            }
            _ => return Err(Error::local_error(ErrorKind::UnsupportedParam)),
        };
        self.context.flush_context(key_handle)?;

        Ok(key)
    }

    /// Sign a digest with an existing key.
    ///
    /// Takes the key as a parameter, signs and returns the signature.
    ///
    /// # Errors
    /// * errors are returned if any method calls return an error: `Context::context_load`,
    /// `Context::sign`, `Context::flush_context`, `TransientObjectContext::set_session_attrs`
    /// `Context::set_handle_auth`
    pub fn sign(
        &mut self,
        key_context: TpmsContext,
        key_auth: &[u8],
        digest: &[u8],
    ) -> Result<utils::Signature> {
        self.set_session_attrs()?;
        let key_handle = self.context.context_load(key_context)?;
        self.context
            .set_handle_auth(key_handle, key_auth)
            .or_else(|e| {
                self.context.flush_context(key_handle)?;
                Err(e)
            })?;

        let scheme = TPMT_SIG_SCHEME {
            scheme: TPM2_ALG_NULL,
            details: Default::default(),
        };
        let validation = TPMT_TK_HASHCHECK {
            tag: TPM2_ST_HASHCHECK,
            hierarchy: TPM2_RH_NULL,
            digest: Default::default(),
        };
        self.set_session_attrs()?;
        let signature = self
            .context
            .sign(key_handle, digest, scheme, &validation)
            .or_else(|e| {
                self.context.flush_context(key_handle)?;
                Err(e)
            })?;
        self.context.flush_context(key_handle)?;
        Ok(signature)
    }

    /// Verify a signature against a digest.
    ///
    /// Given a digest, a key and a signature, this method returns a `Verified` ticket if the
    /// verification was successful.
    ///
    /// # Errors
    /// * if the verification fails (i.e. the signature is invalid), a TPM error is returned
    /// * errors are returned if any method calls return an error: `Context::context_load`,
    /// `Context::verify_signature`, `Context::flush_context`,
    /// `TransientObjectContext::set_session_attrs`
    pub fn verify_signature(
        &mut self,
        key_context: TpmsContext,
        digest: &[u8],
        signature: utils::Signature,
    ) -> Result<TpmtTkVerified> {
        self.set_session_attrs()?;
        let key_handle = self.context.context_load(key_context)?;

        let signature: TPMT_SIGNATURE = signature.try_into().or_else(|e| {
            self.context.flush_context(key_handle)?;
            Err(e)
        })?;
        self.set_session_attrs()?;
        let verified = self
            .context
            .verify_signature(key_handle, digest, &signature)
            .or_else(|e| {
                self.context.flush_context(key_handle)?;
                Err(e)
            })?;
        self.context.flush_context(key_handle)?;
        Ok(verified.try_into()?)
    }

    /// Sets the encrypt and decrypt flags on the main session used by the context.
    ///
    /// # Errors
    /// * if `Context::set_session_attr` returns an error, that error is propagated through
    fn set_session_attrs(&mut self) -> Result<()> {
        let (session, _, _) = self.context.sessions();
        let session_attr = utils::TpmaSession::new()
            .with_flag(TPMA_SESSION_DECRYPT)
            .with_flag(TPMA_SESSION_ENCRYPT);
        self.context.set_session_attr(session, session_attr)?;
        Ok(())
    }
}
