// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded Bearer discovery parsing, exact single challenge,
// duplicate refusal and redacted metadata/scope; keep HTTP quoted-pair decoding.

use std::collections::BTreeSet;
type AuthParameter<'a> = (&'a str, Option<String>);
type ChallengeStart<'a> = (&'a str, Option<AuthParameter<'a>>);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("OAuth Bearer challenge is malformed, ambiguous, or exceeds its bound")]
pub struct ChallengeError;

#[derive(Default)]
pub struct BearerChallenge {
    metadata: Option<String>,
    scope: Option<String>,
    parameters: BTreeSet<String>,
}
impl std::fmt::Debug for BearerChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BearerChallenge([REDACTED])")
    }
}
impl BearerChallenge {
    pub fn metadata_url(&self) -> Option<&str> {
        self.metadata.as_deref()
    }
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }
    fn add(&mut self, name: &str, value: Option<String>) -> Result<(), ChallengeError> {
        let value = value.ok_or(ChallengeError)?;
        if self.parameters.len() == 32
            || !self.parameters.insert(name.to_ascii_lowercase())
            || value.chars().any(char::is_control)
        {
            return Err(ChallengeError);
        }
        if name.eq_ignore_ascii_case("resource_metadata") {
            let url = url::Url::parse(&value).map_err(|_| ChallengeError)?;
            if value.len() > 8192
                || value.trim() != value
                || url.scheme() != "https"
                || url.host().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(ChallengeError);
            }
            self.metadata = Some(value);
        } else if name.eq_ignore_ascii_case("scope") {
            if value.len() > 4096 || !valid_scope(&value) {
                return Err(ChallengeError);
            }
            self.scope = Some(value);
        }
        Ok(())
    }
    pub fn parse(fields: &[&str]) -> Result<Option<Self>, ChallengeError> {
        if fields.len() > 16 || fields.iter().map(|field| field.len()).sum::<usize>() > 16384 {
            return Err(ChallengeError);
        }
        let mut found = None;
        for field in fields {
            let mut bearer: Option<Self> = None;
            for segment in split_unquoted_segments(field).ok_or(ChallengeError)? {
                if segment.trim().is_empty() {
                    continue;
                }
                if let Some((name, value)) = parse_auth_param(segment) {
                    if let Some(challenge) = bearer.as_mut() {
                        challenge.add(name, value)?;
                    }
                    continue;
                }
                if let Some(challenge) = bearer.take() {
                    if found.replace(challenge).is_some() {
                        return Err(ChallengeError);
                    }
                }
                let (scheme, parameter) = parse_challenge_start(segment).ok_or(ChallengeError)?;
                if scheme.eq_ignore_ascii_case("Bearer") {
                    let mut challenge = Self::default();
                    if let Some((name, value)) = parameter {
                        challenge.add(name, value)?;
                    } else if segment.trim().len() != scheme.len() {
                        return Err(ChallengeError);
                    }
                    bearer = Some(challenge);
                }
            }
            if let Some(challenge) = bearer {
                if found.replace(challenge).is_some() {
                    return Err(ChallengeError);
                }
            }
        }
        Ok(found)
    }
}

fn parse_challenge_start(segment: &str) -> Option<ChallengeStart<'_>> {
    let segment = segment.trim();
    let parameter_start = segment.find(char::is_whitespace);
    let (scheme, parameter) = match parameter_start {
        Some(parameter_start) => (
            &segment[..parameter_start],
            parse_auth_param(&segment[parameter_start..]),
        ),
        None => (segment, None),
    };

    is_http_token(scheme).then_some((scheme, parameter))
}

fn parse_auth_param(segment: &str) -> Option<AuthParameter<'_>> {
    let (name, value) = segment.trim().split_once('=')?;
    let name = name.trim();
    is_http_token(name).then_some((name, parse_auth_param_value(value.trim())))
}

fn parse_auth_param_value(value: &str) -> Option<String> {
    if let Some(quoted_value) = value.strip_prefix('"') {
        let quoted_value = quoted_value.strip_suffix('"')?;
        let mut decoded = String::with_capacity(quoted_value.len());
        let mut characters = quoted_value.chars();
        while let Some(character) = characters.next() {
            if character == '\\' {
                decoded.push(characters.next()?);
            } else if character == '"' {
                return None;
            } else {
                decoded.push(character);
            }
        }
        Some(decoded)
    } else {
        is_http_token(value).then(|| value.to_string())
    }
}

fn split_unquoted_segments(header: &str) -> Option<Vec<&str>> {
    let mut segments = Vec::new();
    let mut segment_start = 0;
    let mut in_quotes = false;
    let mut escaped = false;

    for (position, character) in header.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character.is_control() && character != '\t' {
            return None;
        }
        match character {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                if segments.len() == 63 {
                    return None;
                }
                segments.push(&header[segment_start..position]);
                segment_start = position + character.len_utf8();
            }
            _ => {}
        }
    }

    if in_quotes || escaped {
        None
    } else {
        segments.push(&header[segment_start..]);
        Some(segments)
    }
}

fn valid_scope(scope: &str) -> bool {
    scope.split(' ').all(|token| {
        !token.is_empty()
            && token
                .bytes()
                .all(|byte| matches!(byte, b'!' | b'#'..=b'[' | b']'..=b'~'))
    })
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn challenge_priority_respects_schemes_and_http_quoted_pairs() -> Result<(), ChallengeError> {
        let challenge=BearerChallenge::parse(&[r#"Basic realm="other, realm", Bearer realm="MCP", resource_metadata="https://metadata.example/prm", scope="read write", Digest realm="tail""#])?.ok_or(ChallengeError)?;
        assert_eq!(
            challenge.metadata_url(),
            Some("https://metadata.example/prm")
        );
        assert_eq!(challenge.scope(), Some("read write"));
        assert!(!format!("{challenge:?}").contains("metadata.example"));
        assert!(BearerChallenge::parse(&[
            r#"Bearer error="insufficient_scope", scope="read\ write""#
        ])?
        .is_some());
        assert!(BearerChallenge::parse(&[r#"Basic realm="only""#])?.is_none());
        Ok(())
    }
    #[test]
    fn ambiguous_duplicated_malformed_or_unsafe_challenges_are_refused() {
        for fields in [
            vec![
                r#"Bearer resource_metadata="https://a.example", RESOURCE_METADATA="https://b.example""#,
            ],
            vec![r#"Bearer scope="one""#, r#"Bearer scope="two""#],
            vec![r#"Bearer resource_metadata="http://metadata.example""#],
            vec![r#"Bearer scope="a  b""#],
            vec![r#"Bearer scope="read\\write""#],
            vec![r#"Bearer scope="unfinished"#],
            vec!["Bearer scope=read\r\nInjected: x"],
        ] {
            assert!(
                BearerChallenge::parse(&fields).is_err(),
                "accepted malformed fixture"
            );
        }
        assert!(BearerChallenge::parse(&[&"x".repeat(16385)]).is_err());
    }
}
