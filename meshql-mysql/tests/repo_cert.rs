use meshql_core::testing as cert;
use meshql_mysql::MysqlRepository;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mysql::Mysql;

async fn create_repo() -> (MysqlRepository, impl std::any::Any) {
    let container = Mysql::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(3306).await.unwrap();
    // testcontainers-modules mysql defaults: root with empty password, db = "test"
    let url = format!("mysql://root:@127.0.0.1:{port}/test");
    let table = format!("env_{}", uuid::Uuid::new_v4().simple());
    let repo = MysqlRepository::new_with_table(&url, &table)
        .await
        .unwrap();
    (repo, container)
}

#[tokio::test]
async fn create_should_store_and_return_envelope() {
    let (repo, _c) = create_repo().await;
    cert::test_create_should_store_and_return_envelope(&repo).await;
}

#[tokio::test]
async fn read_should_retrieve_existing_envelope() {
    let (repo, _c) = create_repo().await;
    cert::test_read_should_retrieve_existing_envelope(&repo).await;
}

#[tokio::test]
async fn list_should_retrieve_all_created_envelopes() {
    let (repo, _c) = create_repo().await;
    cert::test_list_should_retrieve_all_created_envelopes(&repo).await;
}

#[tokio::test]
async fn remove_should_delete_envelope() {
    let (repo, _c) = create_repo().await;
    cert::test_remove_should_delete_envelope(&repo).await;
}

#[tokio::test]
async fn create_many_should_store_multiple_envelopes() {
    let (repo, _c) = create_repo().await;
    cert::test_create_many_should_store_multiple_envelopes(&repo).await;
}

#[tokio::test]
async fn read_many_should_retrieve_multiple_envelopes() {
    let (repo, _c) = create_repo().await;
    cert::test_read_many_should_retrieve_multiple_envelopes(&repo).await;
}

#[tokio::test]
async fn remove_many_should_delete_multiple_envelopes() {
    let (repo, _c) = create_repo().await;
    cert::test_remove_many_should_delete_multiple_envelopes(&repo).await;
}

#[tokio::test]
async fn should_allow_multiple_versions_and_temporal_reads() {
    let (repo, _c) = create_repo().await;
    cert::test_temporal_versioning(&repo).await;
}

#[tokio::test]
async fn should_only_list_latest_version() {
    let (repo, _c) = create_repo().await;
    cert::test_list_shows_only_latest_version(&repo).await;
}
