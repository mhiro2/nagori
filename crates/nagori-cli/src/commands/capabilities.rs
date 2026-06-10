use anyhow::Result;
use nagori_ipc::IpcRequest;

use super::{Executor, expect_capabilities};
use crate::OutputFormat;
use crate::output::print_capabilities;

pub async fn run(executor: &Executor, format: OutputFormat) -> Result<()> {
    match executor {
        // A static OS probe — deliberately does not open the DB so users
        // can inspect the host matrix on machines where the SQLite path is
        // misconfigured or unreadable.
        Executor::Local(_) => print_capabilities(&nagori_platform_native::capabilities(), format),
        Executor::Ipc(ctx) => {
            let resp = ctx.client.send(IpcRequest::Capabilities).await?;
            print_capabilities(&expect_capabilities(resp)?, format)
        }
    }
}
