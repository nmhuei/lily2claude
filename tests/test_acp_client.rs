use lily2claude::adapters::acp_client::AcpClient;

#[tokio::test]
async fn test_acp_client_instantiation() {
    let client = AcpClient::new(vec!["commandcode".to_string(), "acp".to_string()]);
    assert_eq!(client.command()[0], "commandcode");
}

#[tokio::test]
async fn test_acp_session_spawn_and_prompt() {
    use lily2claude::engine::session::AcpSession;
    let mut session = AcpSession::spawn(&["commandcode".to_string(), "acp".to_string()]).await.expect("spawn session");
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(async move {
        session.send_prompt("deepseek/deepseek-v4.1-flash", "say test_acp_ok", tx).await.expect("send_prompt");
    });

    let mut output = String::new();
    while let Some(chunk) = rx.recv().await {
        output.push_str(&chunk);
    }
    println!("ACP output: {}", output);
    assert!(!output.is_empty());
}
