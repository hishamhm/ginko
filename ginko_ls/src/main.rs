use clap::Parser;
use ginko_server::Backend;
use tower_lsp::{LspService, Server};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, help = "Add a path to search for include files")]
    include: Option<Vec<String>>,
}

#[tokio::main]
pub async fn main() {
    let args = Args::parse();
    let include_paths = args.include.unwrap_or_default();
    // Log to stderr instead of stdout since communication with the language server client
    // happens through stdout
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let (stdin, stdout) = (tokio::io::stdin(), tokio::io::stdout());

    let (service, socket) = LspService::new(move |client| {
        Backend::new("ginko_ls", client).with_include_paths(include_paths)
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
