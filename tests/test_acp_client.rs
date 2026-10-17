use lily2claude::adapters::acp_client::AcpClient;

#[tokio::test]
async fn test_acp_client_instantiation() {
    let client = AcpClient::new(vec!["commandcode".to_string(), "acp".to_string()]);
    assert_eq!(client.command()[0], "commandcode");
}
