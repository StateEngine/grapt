//! Credential origin refusal is independent of request family and account.
use super::*;

#[tokio::test]
async fn a_bound_credential_refuses_another_origin_before_sending(
) -> Result<(), Box<dyn std::error::Error>> {
    let foreign = serve(200, &[], Vec::new()).await;
    let mut call = input(
        "att_bound_foreign",
        &foreign.base,
        ProtocolFamily::OpenAiCompatible,
        compat_request(),
    );
    call.lease = crate::ProviderCredential::inject(
        call.lease.account().clone(),
        call.lease.credential_version().clone(),
        call.lease.placement(),
        grapt_types::Secret::new("bound-private-key".into()),
    )?
    .bind_destination("https://approved.example")?
    .lease()?;
    assert!(matches!(
        adapter(1).open(call).await,
        Err(crate::CodexError::RequestBuild { .. })
    ));
    assert_eq!(foreign.requests.load(Ordering::SeqCst), 0);
    Ok(())
}
