use super::*;
use serde::Serialize;

async fn refusal(response: Response) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = to_bytes(response.into_body(), 4096).await?;
    assert!(
        !bytes.is_empty(),
        "API01_ENCODING_REFUSAL: response encoding lost its error body"
    );
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value["_tag"], "UnknownError");
    Ok(value)
}

#[tokio::test]
async fn api01_an_oversized_response_names_the_encoded_bound(
) -> Result<(), Box<dyn std::error::Error>> {
    let value = "x".repeat(super::super::super::limits::MAX_PAGE_BYTES);
    let error = refusal(json(StatusCode::OK, &value)).await?;
    assert!(error["message"]
        .as_str()
        .ok_or("message")?
        .contains("8388608"));
    Ok(())
}

#[tokio::test]
async fn api01_a_serializer_failure_keeps_a_typed_error_body(
) -> Result<(), Box<dyn std::error::Error>> {
    struct Broken;
    impl Serialize for Broken {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("private serializer detail"))
        }
    }
    let error = refusal(json(StatusCode::OK, &Broken)).await?;
    assert!(!error.to_string().contains("private serializer detail"));
    Ok(())
}
