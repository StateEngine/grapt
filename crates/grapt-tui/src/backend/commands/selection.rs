//! Apache-2.0, Copyright 2025 OpenAI.
//! Modified by Grapt: offered catalog rows and the existing atomic selection owner.
use super::{Client, Completion, Connection, View};
use grapt_client::wire::{ModelRow, Spelled};

fn picker(done: &mut Completion) {
    done.view = Some(View::Models);
    done.consume = true;
}

fn selected<C: Connection>(client: &mut Client<C>, row: &ModelRow, done: &mut Completion) {
    done.finish(client.select_model(row));
    if done.consume {
        done.notice = format!(
            "Model: {}/{} · effort: {}",
            row.provider_id.spelled(),
            row.id.spelled(),
            crate::models::effort(row),
        );
    }
}

pub(super) fn model<C: Connection>(client: &mut Client<C>, args: &str, done: &mut Completion) {
    let mut args = args.split_whitespace();
    let Some(name) = args.next() else {
        return picker(done);
    };
    let effort = args.next();
    if args.next().is_some() {
        done.notice = "Use /model [provider/model [effort]]; draft kept.".into();
        return;
    }
    let (provider, name) = name
        .split_once('/')
        .map_or((None, name), |(p, m)| (Some(p), m));
    let mut offered = client.models().iter().filter(|row| {
        row.id.spelled() == name
            && provider.is_none_or(|p| row.provider_id.spelled() == p)
            && row.variant.as_ref().map(Spelled::spelled) == effort.filter(|e| *e != "default")
    });
    let Some(row) = offered.next().cloned() else {
        done.notice =
            "Model or effort not offered. Use /model to choose an available entry; draft kept."
                .into();
        return;
    };
    if offered.next().is_some() {
        done.notice =
            "Model name is ambiguous. Use /model provider/model [effort]; draft kept.".into();
        return;
    }
    selected(client, &row, done);
}

pub(super) fn effort<C: Connection>(client: &mut Client<C>, args: &str, done: &mut Completion) {
    let mut args = args.split_whitespace();
    let Some(level) = args.next() else {
        return picker(done);
    };
    if args.next().is_some() {
        done.notice = "Use /effort [level|default|status]; draft kept.".into();
        return;
    }
    let Some(current) = client.model() else {
        done.notice = "Choose a model with /model before setting effort; draft kept.".into();
        return;
    };
    if level == "status" {
        done.consume = true;
        done.view = Some(View::Text(vec![
            "REASONING EFFORT".into(),
            format!(
                "Model: {}/{}",
                current.provider_id.spelled(),
                current.id.spelled()
            ),
            format!("Effort: {}", crate::models::effort(current)),
            "Use /effort to choose an offered model and effort.".into(),
        ]));
        return;
    }
    let row = client
        .models()
        .iter()
        .find(|row| {
            row.provider_id == current.provider_id
                && row.id == current.id
                && row.variant.as_ref().map(Spelled::spelled)
                    == (level != "default").then_some(level)
        })
        .cloned();
    match row {
        Some(row) => selected(client, &row, done),
        None => done.notice = "Effort not offered for this model. Use /effort to choose an available entry; draft kept.".into(),
    }
}
