use tower_lsp::{LspService, Server};
use vox_lsp::VoxLanguageServer;

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(VoxLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
