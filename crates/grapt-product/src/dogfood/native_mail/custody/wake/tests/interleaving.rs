//! Evidence-only normal producer and retained idle-owner regression.
use super::*;
#[cfg(feature = "loopback-provider-testkit")]
use serde_json::{json, Value};
#[cfg(feature = "loopback-provider-testkit")]
use std::collections::BTreeSet;
#[cfg(feature = "loopback-provider-testkit")]
use std::path::Path;
use std::time::Duration;

#[cfg(feature = "loopback-provider-testkit")]
fn packet(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > 131072 {
        return Err("synthetic provider packet exceeds bound".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[ignore = "requires an explicitly supplied isolated normal-provider fixture"]
#[tokio::test]
async fn normal_provider_second_input_with_original_debt() -> TestResult {
    #[cfg(feature = "loopback-provider-testkit")]
    return production_case().await;
    #[cfg(not(feature = "loopback-provider-testkit"))]
    Err(
        "explicit loopback-provider-testkit build required for the normal execution consumer"
            .into(),
    )
}

#[cfg(feature = "loopback-provider-testkit")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn production_case() -> TestResult {
    let path = std::env::var_os("GRAPT_NATIVE_MAIL_TEST_PACKET")
        .ok_or("explicit synthetic provider packet required")?;
    let path = Path::new(&path);
    let initial = packet(path)?;
    let (_root, application, mut driver, session) =
        production_fixture::open(&initial, path).await?;
    let native = application
        .native_mail
        .as_ref()
        .ok_or("retained native owners")?;
    let owner = native
        .snapshot()?
        .first()
        .ok_or("retained owner")?
        .1
        .clone();
    let port = NonZeroU16::new(u16::try_from(
        initial["port"].as_u64().ok_or("fixture port")?,
    )?)
    .ok_or("port")?;
    let scope = {
        let mut context = owner.context.lock().await;
        context.use_test_port(port);
        context
            .current_binding()
            .map_err(|e| e.to_string())?
            .ok_or("binding")?
            .scope
    };
    let history = initial["history"]
        .as_array()
        .ok_or("history selector")?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or("history id"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if history.len() != 640 {
        return Err("multi-slice original history required".into());
    }
    let control = Path::new(initial["control"].as_str().ok_or("control path")?);
    let requests = Path::new(
        initial["model_requests"]
            .as_str()
            .ok_or("actual model request capture")?,
    );
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let witness = async {
        let observed: TestResult=async {
            let mut prefix=false;
            let mut first=false;
            let mut second=false;
            let mut first_request=None;
            for _ in 0..15000 {
                let latest=packet(path)?;
                let mut retained=0;
                let mut debt=0;
                let mut current_settled=false;
                let mut first_settled=false;
                {
                    let held=application.lock_inbox();
                    let mut after=grapt_types::MailboxCursor::START;
                    for page_number in 0..16 {
                        let page=held.store().read_remote_mailbox_for_binding(&scope,after,64,4_194_304)?;
                        for record in page.records {
                            let id=record.ack().source_message().as_str();
                            if history.contains(id) {retained+=1;debt+=usize::from(record.delivery_pending()||record.repair_pending());}
                            if Some(id)==latest["current_first"].as_str() {first_settled=!record.delivery_pending()&&!record.repair_pending();}
                            if Some(id)==latest["current_second"].as_str() {current_settled=!record.delivery_pending()&&!record.repair_pending();}
                        }
                        match page.next_after {Some(next)=>after=next,None=>break}
                        if page_number==15 {return Err("retained history proof exceeds bound".into());}
                    }
                }
                if retained==history.len() {
                    prefix |= owner.context.lock().await.runtime.as_ref().is_some_and(|runtime|runtime.is_live());
                }
                let bytes=std::fs::read(requests)?;
                if bytes.len()>2_097_152 {return Err("actual model request capture exceeds bound".into());}
                let executed: Vec<Value>=serde_json::from_slice(&bytes)?;
                if executed.len()>16 {return Err("model request count exceeds bound".into());}
                let texts=executed.iter().map(serde_json::to_string).collect::<Result<Vec<_>,_>>()?;
                let first_entered=latest["current_first"].as_str().and_then(|id|texts.iter().position(|text|text.contains(id)));
                if first_entered.is_some()&&!first&&!first_settled {return Err("first current model input preceded its own delivery settlement".into());}
                if first_request.is_none() {first_request=first_entered;}
                first |= first_entered.is_some();
                let second_request=latest["current_second"].as_str().and_then(|id|texts.iter().position(|text|text.contains(id)));
                let entered=second_request.is_some();
                if entered&&second_request<=first_request {return Err("second task did not enter a later actual model request".into());}
                if entered&&!second {
                    if !prefix||!first||retained!=history.len()||debt==0||!current_settled {
                        return Err(format!("second executed input did not precede original ACK drain: prefix={prefix} first={first} retained={retained} debt={debt} settled={current_settled}").into());
                    }
                    second=true;
                    eprintln!("second retained-owner provider input entered with {debt} original historical ACKs pending");
                }
                let (wakes_available, pending_inputs) = {
                    let inbox = application.lock_inbox();
                    (inbox.native_mail_wakes_available(&session)?, inbox.pending_inputs(&session)?.len())
                };
                let blocked_connection = owner.wake_blocked.lock().map_err(|_|"wake custody")?.as_ref().map(|blocked|blocked.retry_after_connection);
                let failures = {
                    let inbox = application.lock_inbox();
                    let mut statement = inbox.store().connection().prepare("SELECT type,data FROM event WHERE aggregate_id=?1 AND (type LIKE '%failed%' OR type LIKE '%interrupted%') ORDER BY seq DESC LIMIT 4")?;
                    let rows = statement.query_map([session.as_str()],|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?;
                    let mut failures=Vec::new();
                    for row in rows { let (kind,data)=row?; if data.len()>4096 {return Err("failure diagnostic exceeds its bound".into());} failures.push(json!({"kind":kind,"fact":serde_json::from_str::<Value>(&data)?})); }
                    failures
                };
                let pending=control.with_extension("pending");
                std::fs::write(&pending,serde_json::to_vec(&json!({"prefix_complete":prefix,"first_input":first,"second_input":second,"original_debt":debt,"wakes_available":wakes_available,"executed_inputs":executed.len(),"pending_inputs":pending_inputs,"scheduled":owner.wake_scheduled.load(Ordering::SeqCst),"blocked":blocked_connection.is_some(),"blocked_retry_connection":blocked_connection,"failures":failures}))?)?;
                std::fs::rename(pending,control)?;
                if second&&debt==0 {
                    let inbox=application.lock_inbox();
                    for key in ["current_first","current_second"] {
                        let current=latest[key].as_str().ok_or("current task id")?;
                        let mut statement=inbox.store().connection().prepare("SELECT data FROM event WHERE aggregate_id=?1 AND type='session.input.admitted.1' ORDER BY seq LIMIT 16")?;
                        let facts=statement.query_map([session.as_str()],|row|row.get::<_,String>(0))?;
                        let mut promoted=0;
                        for raw in facts {
                            let raw=raw?;if raw.len()>65536{return Err("input admission exceeds bound".into());}
                            let fact:Value=serde_json::from_str(&raw)?;
                            if fact.pointer("/input/nativeMailWake")==Some(&Value::Bool(true))&&fact.pointer("/input/data/text").and_then(Value::as_str).is_some_and(|text|text.contains(current)) {
                                let input:grapt_types::MessageId=fact["inputID"].as_str().ok_or("current input")?.parse()?;
                                if inbox.promoted_cursor(&session,&input)?.is_some(){promoted+=1;}
                            }
                        }
                        if promoted!=1{return Err(format!("current task has {promoted} promoted canonical inputs after retry").into());}
                    }
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err("second service-owned input or eventual original settlement absent".into())
        }.await;
        let _joined = stop.send(());
        observed
    };
    let (run, observed) = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::join!(
            driver.run_until_shutdown(async {
                let _joined = stopped.await;
                crate::dogfood::lifecycle::StopReason::ServeWindowExpired
            }),
            witness
        )
    })
    .await?;
    observed?;
    run?;
    if owner.context.lock().await.owns_runtime() {
        return Err("retained HTTP owner did not join shutdown".into());
    }
    Ok(())
}

#[tokio::test]
async fn retained_v1_remains_outside_automatic_wake_admission() -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(
        r#"{"scope":"project","project_id":"secode","agent_id":"coder"}"#,
    ))
    .await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let (entered, poll) = tokio::sync::oneshot::channel();
    let (transport, run) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            fake::serve(listener, Some(entered), 1),
            driver.run_until(async {
                let _observed = poll.await;
                let mut plan = native
                    .plan(&session)
                    .unwrap_or_else(|error| panic!("V1 plan: {error}"));
                plan.refresh(StepNumber::new(1).unwrap_or_else(|| panic!("step")))
                    .await
                    .unwrap_or_else(|error| panic!("V1 prepare: {error}"));
                let batch = plan
                    .take_batch()
                    .unwrap_or_else(|error| panic!("V1 batch: {error}"))
                    .unwrap_or_else(|| panic!("V1 settled batch"));
                let first = batch
                    .content()
                    .find("mail-a")
                    .unwrap_or_else(|| panic!("first V1 message"));
                let second = batch
                    .content()
                    .find("mail-b")
                    .unwrap_or_else(|| panic!("second V1 message"));
                assert!(
                    first < second,
                    "V1 retained preparation reversed source admission order"
                );
                plan.finish(false)
                    .await
                    .unwrap_or_else(|error| panic!("V1 finish: {error}"));
            })
        )
    })
    .await?;
    transport?;
    run?;
    assert!(
        application
            .lock_inbox()
            .pending_inputs(&session)?
            .is_empty(),
        "V1 transport acquired V2 automatic wake authority"
    );
    Ok(())
}
