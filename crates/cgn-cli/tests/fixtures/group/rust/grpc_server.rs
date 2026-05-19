use tonic::transport::Server;

mod pb {
    pub mod user_server {
        pub struct UserServiceServer;
        impl UserServiceServer {
            pub fn new<T>(_t: T) -> Self { Self }
        }
    }
}

struct UserSvc;

async fn run() {
    Server::builder()
        .add_service(pb::user_server::UserServiceServer::new(UserSvc))
        .serve("0.0.0.0:50051".parse().unwrap())
        .await
        .unwrap();
}
