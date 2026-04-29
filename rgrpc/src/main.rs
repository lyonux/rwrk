use clap::Parser;
use tonic::{Request, Response, Status};

pub mod echo_pb {
    tonic::include_proto!("echo");
}

use echo_pb::echo_client::EchoClient;

#[derive(Default)]
struct EchoService;

#[tonic::async_trait]
impl echo_pb::echo_server::Echo for EchoService {
    async fn echo(
        &self,
        request: Request<echo_pb::EchoRequest>,
    ) -> Result<Response<echo_pb::EchoResponse>, Status> {
        let msg = request.into_inner().message;
        tracing::info!("grpc echo: {}", msg);
        Ok(Response::new(echo_pb::EchoResponse { message: msg }))
    }
}

#[derive(Parser)]
#[command(name = "rgrpc")]
struct Cli {
    /// Run as gRPC server
    #[arg(short = 's')]
    server: bool,

    /// gRPC server listen port (used with -s)
    #[arg(short = 'l', default_value = "9090")]
    listen: u16,

    /// gRPC server URL, required when not in server mode (e.g. http://127.0.0.1:9090)
    #[arg(short = 'u')]
    url: Option<String>,
}

async fn run_server(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("grpc server listening on {}", addr);
    tonic::transport::Server::builder()
        .add_service(echo_pb::echo_server::EchoServer::new(EchoService))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
        .await?;
    Ok(())
}

async fn run_client(url: String) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = EchoClient::connect(url).await?;
    tracing::info!("connected to gRPC server");

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    use tokio::io::AsyncBufReadExt;
    let mut lines = stdin.lines();

    println!("type a message and press enter (ctrl-c to quit):");

    while let Some(line) = lines.next_line().await? {
        let request = tonic::Request::new(echo_pb::EchoRequest { message: line });
        let response = client.echo(request).await?;
        println!("{}", response.into_inner().message);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // Validate: -s and -u are mutually exclusive
    if cli.server && cli.url.is_some() {
        eprintln!("error: -s and -u cannot be used together");
        std::process::exit(1);
    }
    // Without -s, -u is required
    if !cli.server && cli.url.is_none() {
        eprintln!("error: -u is required when not running in server mode");
        std::process::exit(1);
    }

    if cli.server {
        run_server(cli.listen).await
    } else {
        run_client(cli.url.unwrap()).await
    }
}
