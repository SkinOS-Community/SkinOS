//! Developer integration harness. Relays only SkinOs's explicitly created
//! Keychain session to its runtime host over a pipe; never prints the token.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use skinos_core::session;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use zeroize::Zeroize;

#[tokio::main]
async fn main() -> Result<()> {
    let account = session::list_accounts()?
        .into_iter()
        .next()
        .context("Sign in to SkinOs first")?;
    let token = session::load_refresh_token(&account.steamid)?;
    let executable = std::env::current_exe()?
        .parent()
        .context("Executable parent")?
        .parent()
        .context("Target profile")?
        .join("skinos-runtime-host");
    let mut child = tokio::process::Command::new(executable)
        .arg("run")
        .arg("--state-root")
        .arg(session::data_dir()?.join("runtime"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut request = json!({"protocol_version":1,"command":"authenticate","appid":1942280,"steamid":account.steamid,
        "account_name":account.account_name,"refresh_token":token.expose_secret()});
    let mut bytes = serde_json::to_vec(&request)?;
    if let Value::String(mut secret) = request["refresh_token"].take() {
        secret.zeroize()
    }
    bytes.push(b'\n');
    let mut input = child.stdin.take().context("Host stdin")?;
    let written = input.write_all(&bytes).await;
    bytes.zeroize();
    written?;
    let output = child.stdout.take().context("Host stdout")?;
    let mut lines = BufReader::new(output).lines();
    let mut authenticated = false;
    let mut statuses = Vec::new();
    tokio::time::timeout(Duration::from_secs(70), async {
        while let Some(line) = lines.next_line().await? {
            if let Ok(event) = serde_json::from_str::<Value>(&line) {
                let name = event["event"].as_str().unwrap_or("unknown");
                let payload = &event["payload"];
                let state = payload["state"].as_str().unwrap_or("unknown");
                let identity_matches =
                    payload["steamid"].as_str() == Some(account.steamid.as_str());
                if state == "authenticated" && identity_matches {
                    authenticated = true;
                }
                let status = json!({"event":name,"state":state,"identity_matches":identity_matches,
                    "message":payload["message"].as_str().unwrap_or("")});
                println!("{status}");
                statuses.push(status);
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Runtime auth test timed out")??;
    drop(input);
    let exit = child.wait().await?;
    let evidence = json!({"authenticated":authenticated,"host_exit_success":exit.success(),"statuses":statuses});
    std::fs::create_dir_all("outputs")?;
    std::fs::write(
        "outputs/mvp-runtime-auth.json",
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    ensure!(
        authenticated && exit.success(),
        "Runtime did not confirm an authenticated session"
    );
    Ok(())
}
