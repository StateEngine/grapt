use super::*;
use grapt_schema::Optional;

impl Connect {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            if self.cancel_failed && !self.busy {
                self.unconfirmed = self.pane.as_ref().and_then(|p| match &p.screen {
                    Screen::Attempt(attempt) => Some(attempt.clone()),
                    _ => None,
                });
                self.pane = None;
                self.cancel_failed = false;
                self.notice = Some("Sign-in cancellation was not confirmed; the attempt may remain pending until it expires. Ctrl+A retries.".into());
                return;
            }
            if self.busy {
                self.dismiss = true;
                if let Some(p) = &mut self.pane {
                    p.notice = "Cancelling…".into();
                }
            } else if self
                .pane
                .as_ref()
                .is_some_and(|p| matches!(p.screen, Screen::Attempt(_)))
            {
                self.dismiss = true;
                self.request(Request::Cancel);
            } else if let Some(pane) = &mut self.pane {
                match pane.screen {
                    Screen::Providers => self.pane = None,
                    Screen::Choices => {
                        pane.screen = Screen::Providers;
                        pane.provider = None;
                        pane.reset_input();
                    }
                    _ => {
                        pane.screen = Screen::Choices;
                        pane.reset_input();
                    }
                }
            }
            return;
        }
        if self.busy {
            return;
        }
        if key.code == KeyCode::Enter {
            self.enter();
            return;
        }
        let entries = self.entries();
        let Some(pane) = &mut self.pane else {
            return;
        };
        if let Screen::Gateway(draft) = &mut pane.screen {
            if let Err(error) = draft.key(key) {
                pane.notice = error;
            }
            return;
        }
        match &pane.screen {
            Screen::Attempt(attempt)
                if key.code == KeyCode::Char('o') && key.modifiers == KeyModifiers::CONTROL =>
            {
                let url = attempt.url().get().as_str().into();
                self.request(Request::Open(url));
                return;
            }
            Screen::Key if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) => {
                let count = if pane.endpoint_inputs()
                    || pane.bedrock()
                    || pane.databricks()
                    || pane.azure()
                    || pane.is_cloud()
                {
                    5
                } else {
                    2
                };
                pane.key_field = if key.code == KeyCode::BackTab
                    || key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    (pane.key_field + count - 1) % count
                } else {
                    (pane.key_field + 1) % count
                };
                if (pane.azure_cli || pane.google_adc) && pane.key_field == 0 {
                    pane.key_field = if key.code == KeyCode::BackTab
                        || key.modifiers.contains(KeyModifiers::SHIFT)
                    {
                        4
                    } else {
                        1
                    };
                }
                return;
            }
            _ => {}
        }
        let count = match &pane.screen {
            Screen::Providers => entries.len(),
            Screen::Choices => pane.choices().len(),
            Screen::Prompt { method, index, .. } => method
                .prompts()
                .as_present()
                .and_then(|p| p.as_slice().get(*index))
                .and_then(|p| match p {
                    IntegrationPromptDto::Select(s) => Some(s.options().get().as_slice().len()),
                    _ => None,
                })
                .unwrap_or(0),
            _ => 0,
        };
        match key.code {
            KeyCode::Up if count > 0 => pane.selected = pane.selected.saturating_sub(1),
            KeyCode::Down if count > 0 => pane.selected = (pane.selected + 1).min(count - 1),
            KeyCode::PageUp => pane.selected = pane.selected.saturating_sub(pane.page),
            KeyCode::PageDown if count > 0 => {
                pane.selected = (pane.selected + pane.page).min(count - 1)
            }
            KeyCode::Backspace => {
                if matches!(pane.screen, Screen::Key) {
                    pane.key_input().pop();
                } else {
                    pane.input.pop();
                }
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.paste(&ch.to_string());
            }
            _ => {}
        }
    }
    pub fn paste(&mut self, text: &str) {
        if self.busy {
            return;
        }
        let Some(pane) = &mut self.pane else {
            return;
        };
        if let Screen::Gateway(draft) = &mut pane.screen {
            if let Err(error) = draft.paste(text) {
                pane.notice = error;
            }
            return;
        }
        if matches!(pane.screen, Screen::Choices) {
            return;
        }
        if let Screen::Attempt(attempt) = &pane.screen {
            if *attempt.mode().get() != IntegrationOAuthMode::Code {
                return;
            }
        }
        if let Screen::Prompt { method, index, .. } = &pane.screen {
            if method
                .prompts()
                .as_present()
                .and_then(|p| p.as_slice().get(*index))
                .is_some_and(|p| matches!(p, IntegrationPromptDto::Select(_)))
            {
                return;
            }
        }
        let limit =
            if matches!(pane.screen, Screen::Key | Screen::Attempt(_)) && pane.key_field == 0 {
                16_384
            } else if matches!(pane.screen, Screen::Key) && pane.key_field == 2 {
                2048
            } else {
                256
            };
        let service_account =
            pane.is_vertex() && matches!(pane.screen, Screen::Key) && pane.key_field == 0;
        let field = if matches!(pane.screen, Screen::Key) {
            pane.key_input()
        } else {
            &mut pane.input
        };
        if field.len().saturating_add(text.len()) > limit {
            pane.notice = "Input is too long.".into();
            return;
        }
        field.extend(
            text.chars()
                .filter(|c| !c.is_control() || service_account && matches!(c, '\n' | '\r' | '\t')),
        );
        if matches!(pane.screen, Screen::Providers) {
            pane.selected = 0;
            pane.offset = 0;
        }
    }
    fn enter(&mut self) {
        if let Err(error) = self.submit() {
            self.error(error);
        }
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn submit(&mut self) -> Result<(), String> {
        let entries = self.entries();
        let Some(pane) = &mut self.pane else {
            return Ok(());
        };
        let request = match &mut pane.screen {
            Screen::Providers => {
                let Some(provider) = entries
                    .get(pane.selected)
                    .and_then(|i| self.providers.get(*i))
                    .cloned()
                else {
                    return Ok(());
                };
                pane.provider = Some(provider);
                pane.screen = Screen::Choices;
                pane.reset_input();
                if pane.choices().len() == 1 {
                    return self.submit();
                }
                return Ok(());
            }
            Screen::Choices => {
                let choice = pane.choices().into_iter().nth(pane.selected);
                pane.reset_input();
                match choice {
                    Some(Choice::AzureCli) => {
                        pane.azure_cli = true;
                        pane.key_field = 1;
                        pane.screen = Screen::Key;
                        return Ok(());
                    }
                    Some(Choice::GoogleAdc) => {
                        pane.google_adc = true;
                        pane.key_field = 1;
                        pane.screen = Screen::Key;
                        return Ok(());
                    }
                    Some(Choice::AwsChain) => {
                        pane.aws_chain = true;
                        pane.screen = Screen::Key;
                        return Ok(());
                    }
                    Some(Choice::Key(_)) => {
                        pane.screen = if pane.provider.as_ref().is_some_and(|provider| {
                            provider.id().get().as_str() == "cloudflare-ai-gateway"
                        }) {
                            Screen::Gateway(cloudflare::Draft::default())
                        } else {
                            Screen::Key
                        };
                        return Ok(());
                    }
                    Some(Choice::OAuth(method)) => {
                        pane.screen = Screen::Prompt {
                            method,
                            index: 0,
                            inputs: BTreeMap::new(),
                        };
                        return self.advance_prompt(false);
                    }
                    Some(Choice::Rename(id, label)) => {
                        pane.input = label;
                        pane.screen = Screen::Rename(id);
                        return Ok(());
                    }
                    Some(Choice::Remove(id, _)) => {
                        pane.screen = Screen::Remove(id);
                        return Ok(());
                    }
                    None => return Ok(()),
                }
            }
            Screen::Key => {
                let id = pane
                    .provider
                    .as_ref()
                    .ok_or("No provider selected")?
                    .id()
                    .get()
                    .clone();
                if pane.input.trim().is_empty()
                    && !pane.google_adc
                    && !pane.aws_chain
                    && !pane.azure_cli
                    && !pane.custom()
                    && !pane.endpoint_provider().is_some_and(|row| {
                        row.permits_empty_local_key && pane.endpoint_base().starts_with("http://")
                    })
                {
                    return Err(if pane.is_vertex() {
                        "Paste the service-account JSON key."
                    } else {
                        "Enter an API key."
                    }
                    .into());
                }
                let key = dto_string(if pane.aws_chain || pane.azure_cli || pane.google_adc {
                    ""
                } else {
                    pane.input.trim()
                })?;
                let bedrock = if pane.bedrock() {
                    if pane.region.trim().is_empty() || pane.model.trim().is_empty() {
                        return Err("Enter a region and model ID.".into());
                    }
                    Optional::present(BedrockProviderRequestDto {
                        auth: if pane.aws_chain {
                            Optional::present(grapt_api::u3a::BedrockAuthRequestDto::AwsChain {
                                profile: if pane.input.trim().is_empty() {
                                    Optional::absent()
                                } else {
                                    Optional::present(dto_string(pane.input.trim())?)
                                },
                            })
                        } else {
                            Optional::absent()
                        },
                        region: dto_string(pane.region.trim())?,
                        model: dto_string(pane.model.trim())?,
                        endpoint: if pane.endpoint.trim().is_empty() {
                            Optional::absent()
                        } else {
                            Optional::present(dto_string(pane.endpoint.trim())?)
                        },
                    })
                } else {
                    Optional::absent()
                };
                let (custom, databricks) = if pane.databricks() {
                    let context_window = pane
                        .context
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|context| (512..=16_777_216).contains(context))
                        .ok_or("Enter the model's context window in tokens (512 to 16777216).")?;
                    if pane.endpoint.trim().is_empty() || pane.model.trim().is_empty() {
                        return Err("Enter the workspace URL and model ID.".into());
                    }
                    (
                        Optional::absent(),
                        Optional::present(DatabricksProviderRequestDto {
                            workspace: dto_string(pane.endpoint.trim())?,
                            model: dto_string(pane.model.trim())?,
                            context_window,
                        }),
                    )
                } else if pane.endpoint_inputs() {
                    (
                        Optional::present(pane.endpoint_request()?),
                        Optional::absent(),
                    )
                } else {
                    (Optional::absent(), Optional::absent())
                };
                let azure = if pane.azure() {
                    if pane.endpoint.trim().is_empty()
                        || pane.model.trim().is_empty()
                        || pane.deployment.trim().is_empty()
                    {
                        return Err(
                            "Enter an Azure endpoint, catalog model and deployment name.".into(),
                        );
                    }
                    Optional::present(AzureProviderRequestDto {
                        auth: Optional::present(if pane.azure_cli {
                            grapt_api::u3a::AzureAuthRequestDto::AzureCli
                        } else {
                            grapt_api::u3a::AzureAuthRequestDto::ApiKey
                        }),
                        endpoint: dto_string(pane.endpoint.trim())?,
                        model: dto_string(pane.model.trim())?,
                        deployment: dto_string(pane.deployment.trim())?,
                    })
                } else {
                    Optional::absent()
                };
                let label = if pane.label.trim().is_empty() {
                    Optional::absent()
                } else {
                    Optional::present(dto_string(pane.label.trim())?)
                };
                let vertex = if pane.is_vertex() {
                    Optional::present(grapt_api::u3a::VertexProviderRequestDto {
                        auth: if pane.google_adc {
                            Optional::present(
                                grapt_api::u3a::VertexAuthRequestDto::ApplicationDefault,
                            )
                        } else {
                            Optional::absent()
                        },
                        project: dto_string(pane.project.trim())?,
                        location: dto_string(pane.location.trim())?,
                        model: dto_string(pane.model.trim())?,
                    })
                } else {
                    Optional::absent()
                };
                let watsonx = if pane.is_watsonx() {
                    Optional::present(grapt_api::u3a::WatsonxProviderRequestDto {
                        project: dto_string(pane.project.trim())?,
                        region: dto_string(pane.location.trim())?,
                        model: dto_string(pane.model.trim())?,
                    })
                } else {
                    Optional::absent()
                };
                if !pane.aws_chain {
                    pane.input.clear();
                }
                Request::Key(
                    id,
                    Box::new(IntegrationKeyConnectRequestDto {
                        key,
                        label,
                        vertex,
                        watsonx,
                        cloudflare: Optional::absent(),
                        custom,
                        bedrock,
                        databricks,
                        azure,
                    }),
                )
            }
            Screen::Gateway(draft) => {
                let id = pane
                    .provider
                    .as_ref()
                    .ok_or("No provider selected")?
                    .id()
                    .get()
                    .clone();
                Request::Key(id, Box::new(draft.submit()?))
            }
            Screen::Prompt { .. } => return self.advance_prompt(true),
            Screen::Attempt(attempt) => {
                if *attempt.mode().get() != IntegrationOAuthMode::Code {
                    return Ok(());
                }
                Request::Complete(IntegrationOAuthCompleteRequestDto {
                    code: Optional::present(dto_string(&pane.input)?),
                })
            }
            Screen::Rename(id) => {
                if pane.input.trim().is_empty() {
                    return Err("Enter a connection label.".into());
                }
                Request::Rename(
                    id.clone(),
                    CredentialUpdateRequestDto::new(grapt_schema::Required::new(dto_string(
                        pane.input.trim(),
                    )?)),
                )
            }
            Screen::Remove(id) => {
                if pane.input != "disconnect" {
                    return Err("Type disconnect to confirm.".into());
                }
                Request::Remove(id.clone())
            }
        };
        self.request(request);
        Ok(())
    }
    fn advance_prompt(&mut self, answered: bool) -> Result<(), String> {
        let pane = self.pane.as_mut().ok_or("Connection dialog closed")?;
        let Screen::Prompt {
            method,
            index,
            inputs,
        } = &mut pane.screen
        else {
            return Ok(());
        };
        let prompts = method
            .prompts()
            .as_present()
            .map(|p| p.as_slice())
            .unwrap_or_default();
        if answered {
            if let Some(prompt) = prompts.get(*index) {
                let (key, value) = match prompt {
                    IntegrationPromptDto::Text(t) => (t.key().get().as_str(), pane.input.clone()),
                    IntegrationPromptDto::Select(s) => (
                        s.key().get().as_str(),
                        s.options()
                            .get()
                            .as_slice()
                            .get(pane.selected)
                            .ok_or("Choose an option")?
                            .value()
                            .get()
                            .as_str()
                            .into(),
                    ),
                };
                inputs.insert(key.into(), value);
                *index += 1;
                pane.input.clear();
                pane.selected = 0;
            }
        }
        while let Some(prompt) = prompts.get(*index) {
            let when = match prompt {
                IntegrationPromptDto::Text(t) => t.when().as_present(),
                IntegrationPromptDto::Select(s) => s.when().as_present(),
            };
            if condition(when, inputs) {
                return Ok(());
            }
            *index += 1;
        }
        let id = pane
            .provider
            .as_ref()
            .ok_or("No provider selected")?
            .id()
            .get()
            .clone();
        let body: IntegrationOAuthConnectRequestDto = serde_json::from_value(
            serde_json::json!({"methodID":method.id().get().as_str(), "inputs":inputs}),
        )
        .map_err(|_| "Invalid sign-in answers")?;
        self.request(Request::OAuth(id, body));
        Ok(())
    }
}
impl Pane {
    /// An endpoint connect: a model and its window are optional, and an
    /// endpoint named without a model offers every model it serves.
    fn endpoint_request(&self) -> Result<CustomProviderRequestDto, String> {
        let model = self.model.trim();
        let context = self.context.trim();
        let context_window = match context {
            "" => Optional::absent(),
            _ if model.is_empty() => {
                return Err("Enter the model ID that this context window belongs to.".into())
            }
            _ => Optional::present(
                context
                    .parse::<u32>()
                    .ok()
                    .filter(|context| (512..=16_777_216).contains(context))
                    .ok_or("Enter the model's context window in tokens (512 to 16777216).")?,
            ),
        };
        let endpoint = self.endpoint_base();
        if endpoint.is_empty() {
            return Err("Enter the API base URL.".into());
        }
        Ok(CustomProviderRequestDto {
            endpoint: dto_string(endpoint)?,
            model: if model.is_empty() {
                Optional::absent()
            } else {
                Optional::present(dto_string(model)?)
            },
            context_window,
        })
    }
}
fn dto_string(value: &str) -> Result<grapt_api::u3a::DtoString, String> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| "Invalid connection input".into())
}
