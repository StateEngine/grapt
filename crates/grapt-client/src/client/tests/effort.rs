//! Offered effort selection, atomic coordinates and resumed session truth.
use super::*;
#[test]
fn effort_choice_is_offered_and_atomic_on_create_and_switch(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(crate::double::ContractDouble::new(), Bounds::PRODUCTION);
    client.select_first_offer()?;
    let base = client.model().cloned().ok_or("model")?;
    let mut high = base.clone();
    high.variant = Some(grapt_api::u3a::WireId::parse("high".to_owned()).ok_or("effort")?);
    let project = client.project().ok_or("project")?;
    let bytes = crate::wire::encode_models(project, client.models())?;
    let mut catalog: serde_json::Value = serde_json::from_slice(&bytes)?;
    catalog["data"][0]["variants"] = serde_json::json!([{"id":"high"}]);
    client.models = crate::wire::decode_models(&serde_json::to_vec(&catalog)?)?;
    assert!(client.models.iter().any(|row| row.variant == high.variant));
    client.set_launch_effort(Some("high".to_owned()));
    client.select_model(&base)?;
    assert_eq!(
        client
            .model()
            .and_then(|model| model.variant.as_ref())
            .map(|effort| effort.as_inner().as_str()),
        Some("high")
    );
    let agent = client.agent().ok_or("agent")?;
    let create = crate::wire::encode_session_create_choice(
        agent,
        &high.id,
        &high.provider_id,
        "/repo",
        high.variant.as_ref(),
    )?;
    let switch =
        crate::wire::encode_model_choice(&high.id, &high.provider_id, high.variant.as_ref())?;
    for bytes in [create, switch] {
        let body: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["model"]["variant"], "high");
        assert_eq!(body["model"]["id"], high.id.as_inner().as_str());
        assert_eq!(
            body["model"]["providerID"],
            high.provider_id.as_inner().as_str()
        );
    }
    let session = client.create_session()?;
    client.select_model(&base)?;
    client.select(&session)?;
    assert!(client.model().is_some_and(|model| model.variant.is_none()));
    client.select_model(&high)?;
    let second = client.create_session()?;
    client.select_model(&base)?;
    client.select(&session)?;
    assert_eq!(
        client.model().and_then(|model| model.variant.as_ref()),
        high.variant.as_ref()
    );
    client.select(&second)?;
    assert!(client.model().is_some_and(|model| model.variant.is_none()));
    client.select(&session)?;
    client.set_launch_effort(Some("unsupported".to_owned()));
    assert!(client.select_model(&base).is_err());
    assert_eq!(client.model(), Some(&high));
    Ok(())
}
