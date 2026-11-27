//! Gateway input is separate from the masked token and is never a header map.
use super::*;
use grapt_api::u3a::{DtoString, IntegrationCloudflareGatewayConnectDto};
use grapt_schema::Optional;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Field {
    #[default]
    Token,
    Label,
    Account,
    Gateway,
    Provider,
}
#[derive(Default)]
pub(super) struct Draft {
    token: String,
    label: String,
    account: String,
    gateway: String,
    provider: String,
    field: Field,
}
impl Draft {
    fn active(&mut self) -> &mut String {
        match self.field {
            Field::Token => &mut self.token,
            Field::Label => &mut self.label,
            Field::Account => &mut self.account,
            Field::Gateway => &mut self.gateway,
            Field::Provider => &mut self.provider,
        }
    }
    pub(super) fn key(&mut self, key: KeyEvent) -> Result<(), String> {
        match key.code {
            KeyCode::Tab => {
                self.field = match self.field {
                    Field::Token => Field::Label,
                    Field::Label => Field::Account,
                    Field::Account => Field::Gateway,
                    Field::Gateway => Field::Provider,
                    Field::Provider => Field::Token,
                }
            }
            KeyCode::BackTab => {
                self.field = match self.field {
                    Field::Token => Field::Provider,
                    Field::Provider => Field::Gateway,
                    Field::Gateway => Field::Account,
                    Field::Account => Field::Label,
                    Field::Label => Field::Token,
                }
            }
            KeyCode::Backspace => {
                self.active().pop();
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.paste(&ch.to_string())?;
            }
            _ => {}
        }
        Ok(())
    }
    pub(super) fn paste(&mut self, text: &str) -> Result<(), String> {
        let cap = if self.field == Field::Token {
            16_384
        } else {
            256
        };
        let active = self.active();
        if active.len().saturating_add(text.len()) > cap {
            return Err("Input is too long.".into());
        }
        active.extend(text.chars().filter(|ch| !ch.is_control()));
        Ok(())
    }
    pub(super) fn submit(&mut self) -> Result<IntegrationKeyConnectRequestDto, String> {
        let text = |value: &str| {
            DtoString::try_new(value.trim()).map_err(|_| "Invalid connection input".to_owned())
        };
        if [&self.token, &self.account, &self.gateway, &self.provider]
            .iter()
            .any(|s| s.trim().is_empty())
        {
            return Err("Enter a token, account ID, gateway ID and model provider.".into());
        }
        let request = IntegrationKeyConnectRequestDto {
            key: text(&self.token)?,
            label: if self.label.trim().is_empty() {
                Optional::absent()
            } else {
                Optional::present(text(&self.label)?)
            },
            custom: Optional::absent(),
            bedrock: Optional::absent(),
            azure: Optional::absent(),
            databricks: Optional::absent(),
            vertex: Optional::absent(),
            watsonx: Optional::absent(),
            cloudflare: Optional::present(IntegrationCloudflareGatewayConnectDto {
                account: text(&self.account)?,
                gateway: text(&self.gateway)?,
                provider: text(&self.provider)?,
            }),
        };
        self.token.clear();
        Ok(request)
    }
    pub(super) fn draw(&self, lines: &mut Vec<ratatui::text::Line<'static>>) {
        use grapt_tui_transcript::markdown::literal;
        use ratatui::text::Line;
        lines.push(Line::from(
            "Enter a Cloudflare API token with access to this account.",
        ));
        for (field, name, value) in [
            (
                Field::Token,
                "API token",
                "•".repeat(self.token.chars().count().min(48)),
            ),
            (Field::Label, "Label (optional)", literal(&self.label)),
            (Field::Account, "Account ID", literal(&self.account)),
            (Field::Gateway, "Gateway ID", literal(&self.gateway)),
            (Field::Provider, "Model provider", literal(&self.provider)),
        ] {
            lines.push(Line::from(format!(
                "{}{name}: {value}",
                if self.field == field { "› " } else { "  " }
            )));
        }
        lines.push(Line::from(
            "Providers: openai, anthropic, google, xai, alibaba, meta,",
        ));
        lines.push(Line::from(
            "unbiased, moonshotai, fireworks, reflection-ai, minimax, deepseek",
        ));
        lines.push(Line::from(
            "OpenAI/Anthropic: AI Gateway Run; stored keys or Unified Billing.",
        ));
        lines.push(Line::from(
            "Other providers: Workers AI Read; Cloudflare credits.",
        ));
        lines.push(Line::from(
            "Tab/Shift+Tab switches fields · Enter checks and saves",
        ));
    }
}
