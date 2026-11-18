//! Headless end-to-end check: Flint's discord-api and discord-gateway crates against steeld.
use client_core::{Command, Event, auth::{Failure, SessionSecret}};
use discord_api::{DiscordApi, upload::{Source, Status}};
use model::Id;
use serde_json::{Value, json};
use std::{sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::sync::{mpsc, watch};

const SERVER: &str = "http://127.0.0.1:8099";
const API: &str = "http://127.0.0.1:8099/api/v10";
const WAIT: Duration = Duration::from_secs(20);

type Rx = mpsc::UnboundedReceiver<Event>;

fn step(msg: &str) {
    println!("[e2e] {msg}");
}

async fn rest(method: &str, token: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let client = wreq::Client::new();
    let url = format!("{API}{path}");
    let mut req = match method {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        other => panic!("method {other}"),
    };
    req = req.header("authorization", token);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await.expect("rest send");
    let status = resp.status().as_u16();
    let text = resp.text().await.expect("rest body");
    let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, value)
}

fn start_gateway(secret: Arc<SessionSecret>) -> (Rx, tokio::task::JoinHandle<Result<(), Failure>>, watch::Sender<Option<discord_gateway::MemberSubscription>>) {
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Event>();
    let (sub_tx, sub_rx) = watch::channel::<Option<discord_gateway::MemberSubscription>>(None);
    let url = model::endpoints::get().gateway_url.clone();
    let emit = move |event: Event| -> Result<(), Failure> { ev_tx.send(event).map_err(|_| Failure::Protocol) };
    let handle = tokio::spawn(async move { discord_gateway::run(secret, url, sub_rx, emit).await });
    (ev_rx, handle, sub_tx)
}

async fn wait_for<T>(rx: &mut Rx, label: &str, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
    tokio::time::timeout(WAIT, async {
        loop {
            let event = rx.recv().await.expect("gateway event channel closed");
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"))
}

#[tokio::main]
async fn main() {
    model::endpoints::configure(SERVER).expect("configure server");
    let tag = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() % 100_000_000;
    let alice_name = format!("e2ea{tag}");
    let bob_name = format!("e2eb{tag}");
    let password = "correct-horse-battery-1";

    step("register alice and bob through discord_api::auth");
    let alice = Arc::new(discord_api::auth::register(&alice_name, password, None).await.expect("register alice"));
    let bob = Arc::new(discord_api::auth::register(&bob_name, password, None).await.expect("register bob"));
    let alice_token = alice.expose().to_owned();
    let bob_token = bob.expose().to_owned();
    step("PASS register");

    step("connect both gateways, wait for READY");
    let (mut alice_rx, alice_gw, _alice_sub) = start_gateway(alice.clone());
    let (mut bob_rx, bob_gw, _bob_sub) = start_gateway(bob.clone());
    wait_for(&mut alice_rx, "alice READY", |e| matches!(e, Event::Startup(_)).then_some(())).await;
    wait_for(&mut bob_rx, "bob READY", |e| matches!(e, Event::Startup(_)).then_some(())).await;
    assert!(!alice_gw.is_finished() && !bob_gw.is_finished(), "gateway task ended early");
    step("PASS READY on both");

    step("alice creates a guild and an invite; bob accepts");
    let (st, guild) = rest("POST", &alice_token, "/guilds", Some(json!({"name": "E2E guild"}))).await;
    assert!(st / 100 == 2, "create guild: {guild}");
    let guild_id = guild["id"].as_str().expect("guild id").to_owned();
    let (st, channels) = rest("GET", &alice_token, &format!("/guilds/{guild_id}/channels"), None).await;
    assert!(st / 100 == 2, "list channels: {channels}");
    let general = channels.as_array().unwrap().iter().find(|c| c["type"] == 0).expect("text channel")["id"].as_str().unwrap().to_owned();
    let (st, invite) = rest("POST", &alice_token, &format!("/channels/{general}/invites"), Some(json!({}))).await;
    assert!(st / 100 == 2, "create invite: {invite}");
    let code = invite["code"].as_str().expect("invite code").to_owned();
    let (st, joined) = rest("POST", &bob_token, &format!("/invites/{code}"), None).await;
    assert!(st == 200 || st == 201, "accept invite: {st} {joined}");
    step("PASS guild, invite, accept");

    let channel: Id = general.parse().expect("channel id");
    let api_alice = DiscordApi::new(alice.clone()).expect("alice api");

    step("alice sends text through Flint DiscordApi; bob must receive MESSAGE_CREATE");
    let text = format!("hello bob {tag}");
    let sent = api_alice
        .execute(Command::Send { sticker: None, channel, content: text.clone(), nonce: "e2e-text".into(), reply: None })
        .await;
    match sent {
        Event::SendResult { result: Ok(_), .. } => {}
        _ => panic!("alice send did not succeed"),
    }
    let got = wait_for(&mut bob_rx, "bob MESSAGE_CREATE", |e| match e {
        Event::Message(m) if m.content == text && m.channel == channel => Some(m.author.id),
        _ => None,
    })
    .await;
    step(&format!("PASS MESSAGE_CREATE on bob (author {got})"));

    step("bob replies through Flint; alice must receive it");
    let api_bob = DiscordApi::new(bob.clone()).expect("bob api");
    let reply_text = format!("hi alice {tag}");
    let sent = api_bob
        .execute(Command::Send { sticker: None, channel, content: reply_text.clone(), nonce: "e2e-reply".into(), reply: None })
        .await;
    assert!(matches!(sent, Event::SendResult { result: Ok(_), .. }), "bob send failed");
    wait_for(&mut alice_rx, "alice MESSAGE_CREATE", |e| matches!(e, Event::Message(m) if m.content == reply_text).then_some(())).await;
    step("PASS reply delivered both ways");

    step("alice uploads a file through Flint's signed upload path; bob must get the attachment");
    let body = b"hello from the flint headless run\n".to_vec();
    let source = Source::pasted_text(String::from_utf8(body.clone()).unwrap()).expect("source");
    let (progress_tx, _progress_rx) = watch::channel(Status::Preparing);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let upload_text = format!("with file {tag}");
    let event = api_alice
        .upload_message(Command::Send { sticker: None, channel, content: upload_text.clone(), nonce: "e2e-upload".into(), reply: None }, source, progress_tx, cancel_rx)
        .await;
    let sent_attachments = match event {
        Event::SendResult { result: Ok(m), .. } => m.attachments.len(),
        Event::SendResult { result: Err(e), .. } => panic!("upload failed: {e}"),
        _ => panic!("unexpected upload event"),
    };
    assert_eq!(sent_attachments, 1, "alice's upload returned attachments");
    let (st, listed) = rest("GET", &bob_token, &format!("/channels/{general}/messages?limit=5"), None).await;
    assert!(st / 100 == 2, "bob lists messages: {listed}");
    let found = listed.as_array().unwrap().iter().find(|m| m["content"] == upload_text).expect("uploaded message visible to bob");
    let url = model::endpoints::absolute_url(found["attachments"][0]["url"].as_str().expect("attachment url"));
    wait_for(&mut bob_rx, "bob MESSAGE_CREATE with attachment", |e| matches!(e, Event::Message(m) if m.content == upload_text && m.attachments.len() == 1).then_some(())).await;
    let fetched = wreq::Client::new().get(url.clone()).send().await.expect("fetch cdn");
    assert!(fetched.status().is_success(), "cdn fetch {url}: {}", fetched.status());
    let bytes = fetched.bytes().await.expect("cdn bytes");
    assert_eq!(bytes.as_ref(), body.as_slice(), "downloaded bytes match upload");
    step("PASS upload, MESSAGE_CREATE with attachment, CDN bytes match");

    step("ALL CHECKS PASSED");
    std::process::exit(0);
}
