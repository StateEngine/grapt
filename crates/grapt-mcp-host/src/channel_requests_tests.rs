use super::*;
use serde_json::json;

struct Answer;
impl ServerRequestHandler for Answer {
    fn answer<'a>(
        &'a mut self,
        request: &'a grapt_mcp::incoming::ServerRequest,
        _: Option<&'a grapt_form::global::CallDispatch>,
    ) -> crate::RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(request.method(), "elicitation/create");
            Ok(json!({"action":"accept","content":{"name":"fixture-user"}}))
        })
    }
}

#[tokio::test]
async fn nested_request_is_answered_before_the_correlated_tool_result(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, server) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds).with_elicitation_for_test();
    let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let server = async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await?;
        let outgoing: Value = serde_json::from_str(&line)?;
        write
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"elicitation/create\",\"params\":{}}\n",
            )
            .await?;
        line.clear();
        read.read_line(&mut line).await?;
        let answer: Value = serde_json::from_str(&line)?;
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["content"]["name"], "fixture-user");
        let reply =
            json!({"jsonrpc":"2.0","id":outgoing["id"],"result":{"tools":[]}}).to_string() + "\n";
        write.write_all(reply.as_bytes()).await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let mut answer = Answer;
    let (result, served) = tokio::join!(
        channel.exchange_with(request, Duration::from_secs(1), Some(&mut answer), None),
        server
    );
    served?;
    assert_eq!(
        result.map_err(|error| format!("{error:?}"))?,
        json!({"tools":[]})
    );
    Ok(())
}

struct NeverAnswers;
impl ServerRequestHandler for NeverAnswers {
    fn answer<'a>(
        &'a mut self,
        _: &'a grapt_mcp::incoming::ServerRequest,
        _: Option<&'a grapt_form::global::CallDispatch>,
    ) -> crate::RequestFuture<'a> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn human_wait_outlives_the_active_budget_and_remains_cancellable(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, mut server) = tokio::io::duplex(8192);
    server.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"form-one\",\"method\":\"elicitation/create\",\"params\":{}}\n").await?;
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds).with_elicitation_for_test();
    let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let mut handler = NeverAnswers;
    let mut exchange = Box::pin(channel.exchange_with(
        request,
        Duration::from_millis(10),
        Some(&mut handler),
        None,
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut exchange)
            .await
            .is_err()
    );
    drop(exchange);
    Ok(())
}

struct DelayedAnswer(Duration);
impl ServerRequestHandler for DelayedAnswer {
    fn answer<'a>(
        &'a mut self,
        _: &'a grapt_mcp::incoming::ServerRequest,
        _: Option<&'a grapt_form::global::CallDispatch>,
    ) -> crate::RequestFuture<'a> {
        Box::pin(async move {
            tokio::time::sleep(self.0).await;
            Ok(json!({"action":"accept","content":{"name":"fixture-user"}}))
        })
    }
}

#[tokio::test]
async fn a_healthy_elicitation_reply_does_not_expire_while_the_human_answers(
) -> Result<(), Box<dyn std::error::Error>> {
    let result =
        elicitation_after_delays(Duration::ZERO, Duration::from_millis(150), Duration::ZERO)
            .await?;
    assert_eq!(
        result.map_err(|error| format!("{error:?}"))?,
        json!({"tools":[]})
    );
    Ok(())
}

#[tokio::test]
async fn pausing_for_a_human_preserves_the_servers_previously_spent_active_budget(
) -> Result<(), Box<dyn std::error::Error>> {
    let result = elicitation_after_delays(
        Duration::from_millis(70),
        Duration::from_millis(150),
        Duration::from_millis(70),
    )
    .await?;
    assert!(matches!(result, Err(ChannelError::TimedOut)));
    Ok(())
}

#[tokio::test]
async fn a_timeout_before_the_form_response_does_not_leave_the_fixture_waiting(
) -> Result<(), Box<dyn std::error::Error>> {
    let result =
        elicitation_after_delays(Duration::from_millis(150), Duration::ZERO, Duration::ZERO)
            .await?;
    assert!(matches!(result, Err(ChannelError::TimedOut)));
    Ok(())
}

async fn elicitation_after_delays(
    before: Duration,
    human: Duration,
    after: Duration,
) -> Result<Result<Value, ChannelError>, Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, server) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds).with_elicitation_for_test();
    let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let mut handler = DelayedAnswer(human);
    let serving = async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await?;
        let request: Value = serde_json::from_str(&line)?;
        tokio::time::sleep(before).await;
        write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"human-form\",\"method\":\"elicitation/create\",\"params\":{}}\n").await?;
        line.clear();
        read.read_line(&mut line).await?;
        let answer: Value = serde_json::from_str(&line)?;
        assert_eq!(answer["result"]["action"], "accept");
        tokio::time::sleep(after).await;
        let response =
            json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":[]}}).to_string() + "\n";
        write.write_all(response.as_bytes()).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let exchange = channel.exchange_with(
        request,
        Duration::from_millis(100),
        Some(&mut handler),
        None,
    );
    tokio::pin!(exchange, serving);
    // The simulated server may still be awaiting the form when the client
    // times out. Drop that future as soon as the exchange exits, and bound
    // the complete fixture independently of the behavior under test.
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            result = &mut exchange => Ok::<_, Box<dyn std::error::Error>>(result),
            served = &mut serving => {
                served?;
                Ok(exchange.await)
            }
        }
    })
    .await?
}

#[tokio::test]
async fn a_cancelled_partial_read_preserves_every_byte_for_the_next_read(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, mut server) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds);
    server.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":").await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(1), channel.read_frame())
            .await
            .is_err()
    );
    server.write_all(b"1,\"result\":{}}\n").await?;
    let frame = channel
        .read_frame()
        .await
        .map_err(|error| format!("{error:?}"))?;
    assert_eq!(
        serde_json::from_slice::<Value>(&frame)?,
        json!({"jsonrpc":"2.0","id":1,"result":{}})
    );
    Ok(())
}

#[tokio::test]
async fn disconnect_is_observed_while_the_form_wait_is_pending(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, mut server) = tokio::io::duplex(8192);
    server
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"elicitation/create\",\"params\":{}}\n",
        )
        .await?;
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds).with_elicitation_for_test();
    let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let mut handler = NeverAnswers;
    let mut exchange =
        Box::pin(channel.exchange_with(request, Duration::from_secs(10), Some(&mut handler), None));
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut exchange)
            .await
            .is_err()
    );
    drop(server);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), exchange).await?,
        Err(ChannelError::Disconnected(_))
    ));
    Ok(())
}

#[tokio::test]
async fn nested_server_request_ids_cannot_be_reused_during_one_tool_call(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, mut server) = tokio::io::duplex(8192);
    let frame = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{}}\n";
    server.write_all(frame).await?;
    server.write_all(frame).await?;
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds);
    let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let result = channel.exchange(request, Duration::from_secs(1)).await;
    assert!(
        matches!(result,Err(ChannelError::Protocol(McpProtocolError::Malformed(ref message))) if message.contains("repeated or excessive"))
    );
    Ok(())
}

#[tokio::test]
async fn remote_http_elicitation_pauses_the_body_and_channel_deadlines(
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/mcp", listener.local_addr()?);
    let active = Duration::from_millis(100);
    let client = grapt_mcp_client::HttpClient::new(&endpoint, 8192)?;
    let connection = grapt_mcp_client::connect(
        client,
        grapt_mcp_client::TransportBudgets {
            startup: active,
            catalog: active,
            execution: active,
        },
    )?;
    let bounds = McpBounds::conservative()?;
    let mut channel = McpChannel::new(connection.reader, connection.writer, bounds)
        .with_human_wait(connection.human_wait)
        .with_elicitation_for_test();
    let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let server = async move {
        async fn request(
            stream: &mut tokio::net::TcpStream,
        ) -> Result<Value, Box<dyn std::error::Error>> {
            let mut bytes = Vec::new();
            for _ in 0..8192 {
                bytes.push(stream.read_u8().await?);
                let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                    continue;
                };
                let head = std::str::from_utf8(&bytes[..end])?;
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>())
                    })
                    .transpose()?
                    .ok_or("fixture request lacks content length")?;
                if bytes.len() == end + 4 + length {
                    return Ok(serde_json::from_slice(&bytes[end + 4..])?);
                }
            }
            Err("fixture request exceeded its bound".into())
        }
        let (mut stream, _) = listener.accept().await?;
        let sent = request(&mut stream).await?;
        let nested = "data: {\"jsonrpc\":\"2.0\",\"id\":\"human-one\",\"method\":\"elicitation/create\",\"params\":{}}\n\n";
        let result = format!(
            "data: {}\n\n",
            json!({"jsonrpc":"2.0","id":sent["id"],"result":{"tools":[]}})
        );
        stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}", nested.len()+result.len(), nested).as_bytes()).await?;
        let (mut answer_stream, _) = listener.accept().await?;
        let answer = request(&mut answer_stream).await?;
        assert_eq!(answer["id"], "human-one");
        assert_eq!(answer["result"]["action"], "accept");
        answer_stream
            .write_all(b"HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await?;
        stream.write_all(result.as_bytes()).await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let mut handler = DelayedAnswer(Duration::from_millis(150));
    let (result, served) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            channel.exchange_with(request, active, Some(&mut handler), None),
            server
        )
    })
    .await?;
    served?;
    assert_eq!(
        result.map_err(|error| format!("{error:?}"))?,
        json!({"tools":[]})
    );
    drop(connection.task);
    Ok(())
}

/// SC-REL-01c: the server answers an abandoned request late, then the next.
/// `abandon` leaves the first exchange either by its deadline or by dropping
/// it, as an interrupt does.
async fn late_reply_then_next(
    abandon_by_drop: bool,
    late_id: Option<u64>,
) -> Result<Result<Value, ChannelError>, Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, server) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds);
    let (server_read, mut server_write) = tokio::io::split(server);
    let mut server_read = BufReader::new(server_read);
    let first = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let mut line = String::new();
    if abandon_by_drop {
        let exchange = channel.exchange(first, Duration::from_secs(5));
        tokio::select! {
            _ = exchange => return Err("the held request was answered".into()),
            read = server_read.read_line(&mut line) => { read?; }
        }
    } else {
        let (result, read) = tokio::join!(
            channel.exchange(first, Duration::from_millis(50)),
            server_read.read_line(&mut line)
        );
        read?;
        assert!(matches!(result, Err(ChannelError::TimedOut)));
    }
    let outgoing: Value = serde_json::from_str(&line)?;
    let late = late_id.map_or(outgoing["id"].clone(), Value::from);
    let late = json!({"jsonrpc":"2.0","id":late,"result":{"tools":["late"]}}).to_string() + "\n";
    server_write.write_all(late.as_bytes()).await?;
    let second = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let server = async {
        line.clear();
        server_read.read_line(&mut line).await?;
        let outgoing: Value = serde_json::from_str(&line)?;
        let reply = json!({"jsonrpc":"2.0","id":outgoing["id"],"result":{"tools":["own"]}})
            .to_string()
            + "\n";
        server_write.write_all(reply.as_bytes()).await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (result, served) = tokio::join!(channel.exchange(second, Duration::from_secs(1)), server);
    served?;
    Ok(result)
}

#[tokio::test]
async fn a_late_reply_to_an_abandoned_request_never_answers_the_next(
) -> Result<(), Box<dyn std::error::Error>> {
    for abandon_by_drop in [false, true] {
        let result = late_reply_then_next(abandon_by_drop, None).await?;
        assert_eq!(
            result.map_err(|error| format!("{error:?}"))?,
            json!({"tools":["own"]}),
            "abandoned by drop: {abandon_by_drop}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_reply_to_a_request_never_issued_still_fails_closed(
) -> Result<(), Box<dyn std::error::Error>> {
    let result = late_reply_then_next(false, Some(99)).await?;
    assert!(
        matches!(
            result,
            Err(ChannelError::Protocol(
                McpProtocolError::CorrelationMismatch { replied: 99, .. }
            ))
        ),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn idle_partial_frames_and_late_canceled_replies_do_not_steal_the_next_result(
) -> Result<(), Box<dyn std::error::Error>> {
    let bounds = McpBounds::conservative()?;
    let (client, mut server) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(client);
    let mut channel = McpChannel::new(read, write, bounds);
    server
        .write_all(br#"{"jsonrpc":"2.0","method":"notifications/prompts/"#)
        .await?;
    std::future::poll_fn(|cx| std::task::Poll::Ready(channel.poll_idle(cx)))
        .await
        .map_err(|e| format!("{e:?}"))?;
    server.write_all(b"list_changed\"}\n{\"jsonrpc\":\"2.0\",\"id\":77,\"error\":{\"code\":-32000,\"message\":\"late cancellation\"}}\n").await?;
    channel.outstanding = Some(77);
    std::future::poll_fn(|cx| std::task::Poll::Ready(channel.poll_idle(cx)))
        .await
        .map_err(|e| format!("{e:?}"))?;
    let request = grapt_mcp::tools_list_request(channel.sequence(), bounds)?;
    let serving = async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await?;
        let request: Value = serde_json::from_str(&line)?;
        write
            .write_all(
                (json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":[]}}).to_string()
                    + "\n")
                    .as_bytes(),
            )
            .await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (result, served) = tokio::join!(channel.exchange(request, Duration::from_secs(1)), serving);
    served?;
    assert_eq!(result.map_err(|e| format!("{e:?}"))?, json!({"tools":[]}));
    Ok(())
}
