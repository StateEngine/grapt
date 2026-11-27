//! Renew material without changing a carrier's already-authorized identity.
use std::sync::Arc;
use std::time::Duration;

use crate::{CredentialLease, CredentialOwner, CredentialRenewalRefusal, CredentialRevision};

pub(super) struct Prepared {
    pub request: reqwest::RequestBuilder,
    pub lease: Option<CredentialLease>,
    pub renewal: Option<AttemptRenewal>,
}

pub(super) struct AttemptRenewal {
    owner: Arc<dyn CredentialOwner>,
    revision: CredentialRevision,
    allowance: Duration,
}

pub(super) async fn prepare(
    owner: Option<Arc<dyn CredentialOwner>>,
    request: reqwest::RequestBuilder,
    lease: Option<CredentialLease>,
    allowance: Duration,
) -> Result<Prepared, CredentialRenewalRefusal> {
    let Some(owner) = owner else {
        let request = match &lease {
            Some(lease) => tokio::time::timeout(allowance, lease.authenticate(request))
                .await
                .map_err(|_| CredentialRenewalRefusal::Unavailable)?
                .map_err(|_| CredentialRenewalRefusal::Unavailable)?,
            None => request,
        };
        return Ok(Prepared {
            request,
            lease,
            renewal: None,
        });
    };
    let old = lease.ok_or(CredentialRenewalRefusal::CoordinatesChanged)?;
    let current = tokio::time::timeout(allowance, owner.current())
        .await
        .map_err(|_| CredentialRenewalRefusal::Unavailable)??;
    let credential = current.credential;
    if credential.account() != old.account()
        || credential.credential_version() != old.credential_version()
        || credential.placement() != old.placement()
        || !credential.keeps_destination(&old)
    {
        return Err(CredentialRenewalRefusal::CoordinatesChanged);
    }
    let lease = credential
        .lease()
        .map_err(|_| CredentialRenewalRefusal::Unavailable)?;
    let request = lease
        .place(request)
        .map_err(|_| CredentialRenewalRefusal::CoordinatesChanged)?;
    let request = tokio::time::timeout(allowance, lease.authenticate(request))
        .await
        .map_err(|_| CredentialRenewalRefusal::Unavailable)?
        .map_err(|_| CredentialRenewalRefusal::Unavailable)?;
    Ok(Prepared {
        request,
        lease: Some(lease),
        renewal: Some(AttemptRenewal {
            owner,
            revision: current.revision,
            allowance,
        }),
    })
}

impl AttemptRenewal {
    pub async fn recover(&self, status: u16) -> bool {
        status == 401
            && matches!(
                tokio::time::timeout(self.allowance, self.owner.recover(self.revision)).await,
                Ok(Ok(true))
            )
    }

    pub async fn accepted(&self) {
        let _result =
            tokio::time::timeout(self.allowance, self.owner.accepted(self.revision)).await;
    }
}
