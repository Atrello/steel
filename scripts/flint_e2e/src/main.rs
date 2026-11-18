//! Headless end-to-end check: Flint's discord-api and discord-gateway crates against steeld.
use client_core::{
    Command, Event,
    auth::{Failure, SessionSecret},
};
use discord_api::{
    DiscordApi,
    upload::{Source, Status},
};
use model::Id;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
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
        "PATCH" => client.patch(url),
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

fn start_gateway(
    secret: Arc<SessionSecret>,
) -> (
    Rx,
    tokio::task::JoinHandle<Result<(), Failure>>,
    watch::Sender<Option<discord_gateway::MemberSubscription>>,
) {
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Event>();
    let (sub_tx, sub_rx) = watch::channel::<Option<discord_gateway::MemberSubscription>>(None);
    let url = model::endpoints::get().gateway_url.clone();
    let emit = move |event: Event| -> Result<(), Failure> {
        ev_tx.send(event).map_err(|_| Failure::Protocol)
    };
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


/// Runs one Flint server-admin command, as its UI does.
async fn admin(api: &DiscordApi, guild: Id, action: model::server_admin::Action, request: u64) -> Event {
    api.execute(Command::ServerAdmin { guild, request, action: Box::new(action) }).await
}

#[tokio::main]
async fn main() {
    model::endpoints::configure(SERVER).expect("configure server");
    let tag = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 100_000_000;
    let alice_name = format!("e2ea{tag}");
    let bob_name = format!("e2eb{tag}");
    let password = "correct-horse-battery-1";

    step("register alice and bob through discord_api::auth");
    let alice = Arc::new(
        discord_api::auth::register(&alice_name, password, None)
            .await
            .expect("register alice"),
    );
    let bob = Arc::new(
        discord_api::auth::register(&bob_name, password, None)
            .await
            .expect("register bob"),
    );
    let alice_token = alice.expose().to_owned();
    let bob_token = bob.expose().to_owned();
    step("PASS register");

    step("connect both gateways, wait for READY");
    let (mut alice_rx, alice_gw, _alice_sub) = start_gateway(alice.clone());
    let (mut bob_rx, bob_gw, _bob_sub) = start_gateway(bob.clone());
    wait_for(&mut alice_rx, "alice READY", |e| {
        matches!(e, Event::Startup(_)).then_some(())
    })
    .await;
    wait_for(&mut bob_rx, "bob READY", |e| {
        matches!(e, Event::Startup(_)).then_some(())
    })
    .await;
    assert!(
        !alice_gw.is_finished() && !bob_gw.is_finished(),
        "gateway task ended early"
    );
    step("PASS READY on both");

    step("alice creates a guild and an invite; bob accepts");
    let (st, guild) = rest(
        "POST",
        &alice_token,
        "/guilds",
        Some(json!({"name": "E2E guild"})),
    )
    .await;
    assert!(st / 100 == 2, "create guild: {guild}");
    let guild_id = guild["id"].as_str().expect("guild id").to_owned();
    step("alice's gateway receives GUILD_CREATE (GuildJoined) for the new guild");
    let joined = wait_for(&mut alice_rx, "alice GuildJoined", |e| match e {
        Event::GuildJoined(g) if g.id.0.to_string() == guild_id => Some(()),
        _ => None,
    })
    .await;
    let _ = joined;
    step("PASS GuildJoined on creator");
    let (st, channels) = rest(
        "GET",
        &alice_token,
        &format!("/guilds/{guild_id}/channels"),
        None,
    )
    .await;
    assert!(st / 100 == 2, "list channels: {channels}");
    let general = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == 0)
        .expect("text channel")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (st, invite) = rest(
        "POST",
        &alice_token,
        &format!("/channels/{general}/invites"),
        Some(json!({})),
    )
    .await;
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
        .execute(Command::Send {
            sticker: None,
            channel,
            content: text.clone(),
            nonce: "e2e-text".into(),
            reply: None,
        })
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
        .execute(Command::Send {
            sticker: None,
            channel,
            content: reply_text.clone(),
            nonce: "e2e-reply".into(),
            reply: None,
        })
        .await;
    assert!(
        matches!(sent, Event::SendResult { result: Ok(_), .. }),
        "bob send failed"
    );
    wait_for(&mut alice_rx, "alice MESSAGE_CREATE", |e| {
        matches!(e, Event::Message(m) if m.content == reply_text).then_some(())
    })
    .await;
    step("PASS reply delivered both ways");

    step("alice uploads a file through Flint's signed upload path; bob must get the attachment");
    let body = b"hello from the flint headless run\n".to_vec();
    let source = Source::pasted_text(String::from_utf8(body.clone()).unwrap()).expect("source");
    let (progress_tx, _progress_rx) = watch::channel(Status::Preparing);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let upload_text = format!("with file {tag}");
    let event = api_alice
        .upload_message(
            Command::Send {
                sticker: None,
                channel,
                content: upload_text.clone(),
                nonce: "e2e-upload".into(),
                reply: None,
            },
            source,
            progress_tx,
            cancel_rx,
        )
        .await;
    let sent_attachments = match event {
        Event::SendResult { result: Ok(m), .. } => m.attachments.len(),
        Event::SendResult { result: Err(e), .. } => panic!("upload failed: {e}"),
        _ => panic!("unexpected upload event"),
    };
    assert_eq!(sent_attachments, 1, "alice's upload returned attachments");
    let (st, listed) = rest(
        "GET",
        &bob_token,
        &format!("/channels/{general}/messages?limit=5"),
        None,
    )
    .await;
    assert!(st / 100 == 2, "bob lists messages: {listed}");
    let found = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["content"] == upload_text)
        .expect("uploaded message visible to bob");
    let url = model::endpoints::absolute_url(
        found["attachments"][0]["url"]
            .as_str()
            .expect("attachment url"),
    );
    wait_for(&mut bob_rx, "bob MESSAGE_CREATE with attachment", |e| {
        matches!(e, Event::Message(m) if m.content == upload_text && m.attachments.len() == 1)
            .then_some(())
    })
    .await;
    let fetched = wreq::Client::new()
        .get(url.clone())
        .send()
        .await
        .expect("fetch cdn");
    assert!(
        fetched.status().is_success(),
        "cdn fetch {url}: {}",
        fetched.status()
    );
    let bytes = fetched.bytes().await.expect("cdn bytes");
    assert_eq!(
        bytes.as_ref(),
        body.as_slice(),
        "downloaded bytes match upload"
    );
    step("PASS upload, MESSAGE_CREATE with attachment, CDN bytes match");

    // ----- UI flows: channel create, categories, profile, image upload -----
    let png: Vec<u8> = vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15,
        0, 2, 135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    step(
        "create text, voice, category and a voice channel inside it, through Flint's channel actions",
    );
    use client_core::channel_actions::{Action, CreateKind, Outcome};
    let guild_model: Id = guild_id.parse().expect("guild id");
    let mut request = 1u64;
    let mut run_action = |channel: Id, action: Action| {
        request += 1;
        let api = &api_alice;
        let request = request;
        async move {
            api.execute(Command::ChannelAction {
                guild: guild_model,
                channel,
                request,
                action,
            })
            .await
        }
    };
    let text_event = run_action(
        channel,
        Action::Create {
            name: "e2e-text".into(),
            kind: CreateKind::Text,
        },
    )
    .await;
    let text_id = match text_event {
        Event::ChannelAction(client_core::channel_actions::Event::Finished {
            result: Ok(Outcome::Channel { channel, .. }),
            ..
        }) => channel.id,
        Event::ChannelAction(client_core::channel_actions::Event::Finished {
            result: Err(e),
            ..
        }) => panic!("text channel create failed: {e}"),
        _ => panic!("text channel: unexpected event"),
    };
    let _ = text_id;
    step("PASS text channel create");

    let voice_event = run_action(
        channel,
        Action::Create {
            name: "e2e-voice".into(),
            kind: CreateKind::Voice,
        },
    )
    .await;
    match voice_event {
        Event::ChannelAction(client_core::channel_actions::Event::Finished {
            result: Ok(Outcome::Channel { .. }),
            ..
        }) => {}
        Event::ChannelAction(client_core::channel_actions::Event::Finished {
            result: Err(e),
            ..
        }) => panic!("voice channel create failed: {e}"),
        _ => panic!("voice channel: unexpected event"),
    }
    step("PASS voice channel create (Flint no longer reports Outcome unknown)");

    let category_event = run_action(
        channel,
        Action::CreateCategory {
            name: "e2e-category".into(),
        },
    )
    .await;
    let category_id = match category_event {
        Event::ChannelAction(client_core::channel_actions::Event::Finished {
            result: Ok(Outcome::Channel { channel, .. }),
            ..
        }) => channel.id,
        Event::ChannelAction(client_core::channel_actions::Event::Finished {
            result: Err(e),
            ..
        }) => panic!("category create failed: {e}"),
        _ => panic!("category: unexpected event"),
    };
    let nested = run_action(
        category_id,
        Action::Create {
            name: "e2e-nested".into(),
            kind: CreateKind::Voice,
        },
    )
    .await;
    match &nested {
        Event::ChannelAction(client_core::channel_actions::Event::Finished {
            result: Ok(_),
            ..
        }) => {}
        Event::ChannelAction(client_core::channel_actions::Event::Finished {
            result: Err(e),
            ..
        }) => panic!("voice channel inside category failed: {e}"),
        _ => panic!("nested: unexpected event"),
    }
    step("PASS category and nested voice channel");

    let (st, listed) = rest(
        "GET",
        &alice_token,
        &format!("/guilds/{guild_id}/channels"),
        None,
    )
    .await;
    assert!(st == 200, "list channels: {listed}");
    let kinds: Vec<i64> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["type"].as_i64().unwrap())
        .collect();
    assert!(
        kinds.contains(&2) && kinds.contains(&4),
        "expected voice and category in {kinds:?}"
    );
    step("PASS channel list shows voice and category types");

    step("bob opens alice's profile popout through Flint; it must decode");
    let alice_id = {
        let (st, me) = rest("GET", &alice_token, "/users/@me", None).await;
        assert!(st == 200, "users/@me: {me}");
        me["id"].as_str().unwrap().parse::<Id>().expect("alice id")
    };
    let api_bob_profile = DiscordApi::new(bob.clone()).expect("bob api");
    let profile = api_bob_profile
        .execute(Command::Profile {
            user: alice_id,
            guild: None,
            request: 9,
            with_mutuals: false,
            cosmetics: Default::default(),
        })
        .await;
    match profile {
        Event::Profile { result: Ok(_), .. } => {}
        Event::Profile { result: Err(e), .. } => panic!("profile failed: {e}"),
        _ => panic!("profile: unexpected event"),
    }
    step("PASS profile decodes");

    step("alice uploads a PNG through Flint; bob must get it and the bytes must match");
    let (progress_tx2, _progress_rx2) = watch::channel(Status::Preparing);
    let (_cancel_tx2, cancel_rx2) = watch::channel(false);
    let image_text = format!("photo {tag}");
    let image_source = Source::pasted_png(png.clone()).expect("png source");
    let image_event = api_alice
        .upload_message(
            Command::Send {
                sticker: None,
                channel,
                content: image_text.clone(),
                nonce: "e2e-photo".into(),
                reply: None,
            },
            image_source,
            progress_tx2,
            cancel_rx2,
        )
        .await;
    match image_event {
        Event::SendResult { result: Ok(m), .. } => {
            assert_eq!(m.attachments.len(), 1, "photo attachment")
        }
        Event::SendResult { result: Err(e), .. } => panic!("photo upload failed: {e}"),
        _ => panic!("photo upload: unexpected event"),
    }
    let (st, listed) = rest(
        "GET",
        &bob_token,
        &format!("/channels/{general}/messages?limit=5"),
        None,
    )
    .await;
    assert!(st == 200, "bob lists messages: {listed}");
    let found = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["content"] == image_text)
        .expect("photo message visible");
    let url =
        model::endpoints::absolute_url(found["attachments"][0]["url"].as_str().expect("photo url"));
    let fetched = wreq::Client::new()
        .get(url.clone())
        .send()
        .await
        .expect("fetch photo");
    assert!(
        fetched.status().is_success(),
        "photo fetch {url}: {}",
        fetched.status()
    );
    assert_eq!(
        fetched.bytes().await.expect("photo bytes").as_ref(),
        png.as_slice(),
        "photo bytes match"
    );
    step("PASS photo upload and download");

    // ----- roles and permissions over REST -----
    step("roles: bob cannot create a channel without MANAGE_CHANNELS");
    let (st, _) = rest(
        "POST",
        &bob_token,
        &format!("/guilds/{guild_id}/channels"),
        Some(json!({"name": "bob-denied"})),
    )
    .await;
    assert_eq!(st, 403, "bob should be denied channel create");
    step("PASS denied");

    step(
        "roles: alice creates a role with MANAGE_CHANNELS, assigns it to bob, bob can create a channel",
    );
    let (st, role) = rest(
        "POST",
        &alice_token,
        &format!("/guilds/{guild_id}/roles"),
        Some(json!({"name": "mods", "permissions": "16"})),
    )
    .await;
    assert!(st / 100 == 2, "create role: {role}");
    let role_id = role["id"].as_str().expect("role id").to_owned();
    let bob_id = {
        let (st, me) = rest("GET", &bob_token, "/users/@me", None).await;
        assert!(st == 200, "bob me: {me}");
        me["id"].as_str().unwrap().to_owned()
    };
    let (st, _) = rest(
        "PUT",
        &alice_token,
        &format!("/guilds/{guild_id}/members/{bob_id}/roles/{role_id}"),
        None,
    )
    .await;
    assert!(st / 100 == 2, "assign role: {st}");
    let (st, made) = rest(
        "POST",
        &bob_token,
        &format!("/guilds/{guild_id}/channels"),
        Some(json!({"name": "bob-made"})),
    )
    .await;
    assert!(
        st / 100 == 2,
        "bob with role should create a channel: {made}"
    );
    step("PASS role assignment grants MANAGE_CHANNELS");

    step(
        "roles: guild catalog lists @everyone (guild id) and mods; member counts reflect the assignment",
    );
    let (st, guild_json) = rest("GET", &alice_token, &format!("/guilds/{guild_id}"), None).await;
    assert!(st == 200, "get guild: {guild_json}");
    let roles = guild_json["roles"].as_array().expect("roles array");
    assert!(
        roles
            .iter()
            .any(|r| r["id"] == guild_id.as_str() && r["name"] == "@everyone"),
        "@everyone in {roles:?}"
    );
    assert!(
        roles
            .iter()
            .any(|r| r["id"] == role_id.as_str() && r["name"] == "mods"),
        "mods in {roles:?}"
    );
    let (st, counts) = rest(
        "GET",
        &alice_token,
        &format!("/guilds/{guild_id}/roles/member-counts"),
        None,
    )
    .await;
    assert!(st == 200, "member counts: {counts}");
    assert_eq!(
        counts[role_id.as_str()].as_i64(),
        Some(1),
        "mods member count"
    );
    step("PASS catalog and counts");

    step("overwrites: deny SEND_MESSAGES for @everyone in general; bob is blocked, alice is not");
    let (st, _) = rest("PATCH", &alice_token, &format!("/channels/{general}"), Some(json!({"permission_overwrites": [{"id": guild_id, "type": 0, "allow": "0", "deny": "2048"}]}))).await;
    assert!(st / 100 == 2, "patch overwrite");
    let (st, _) = rest(
        "POST",
        &bob_token,
        &format!("/channels/{general}/messages"),
        Some(json!({"content": "blocked"})),
    )
    .await;
    assert_eq!(st, 403, "bob should be blocked by the overwrite");
    let (st, _) = rest(
        "POST",
        &alice_token,
        &format!("/channels/{general}/messages"),
        Some(json!({"content": "owner still posts"})),
    )
    .await;
    assert!(st / 100 == 2, "owner posts");
    step("PASS @everyone deny");

    step("overwrites: a member allow for bob lifts the block");
    let (st, _) = rest(
        "PATCH",
        &alice_token,
        &format!("/channels/{general}"),
        Some(json!({"permission_overwrites": [
            {"id": guild_id, "type": 0, "allow": "0", "deny": "2048"},
            {"id": bob_id, "type": 1, "allow": "2048", "deny": "0"}
        ]})),
    )
    .await;
    assert!(st / 100 == 2, "patch member overwrite");
    let (st, _) = rest(
        "POST",
        &bob_token,
        &format!("/channels/{general}/messages"),
        Some(json!({"content": "allowed"})),
    )
    .await;
    assert!(st / 100 == 2, "bob allowed by member overwrite: {st}");
    step("PASS member allow overrides @everyone deny");

    step("roles: removing the role takes MANAGE_CHANNELS away again");
    let (st, _) = rest(
        "DELETE",
        &alice_token,
        &format!("/guilds/{guild_id}/members/{bob_id}/roles/{role_id}"),
        None,
    )
    .await;
    assert!(st / 100 == 2, "remove role");
    let (st, _) = rest(
        "POST",
        &bob_token,
        &format!("/guilds/{guild_id}/channels"),
        Some(json!({"name": "bob-again"})),
    )
    .await;
    assert_eq!(st, 403, "bob should lose channel create");
    step("PASS role removal revokes");

    step("roles: non-owners cannot delete the guild, and @everyone cannot be deleted");
    let (st, _) = rest("DELETE", &bob_token, &format!("/guilds/{guild_id}"), None).await;
    assert_eq!(st, 403, "bob cannot delete the guild");
    let (st, _) = rest(
        "DELETE",
        &alice_token,
        &format!("/guilds/{guild_id}/roles/{guild_id}"),
        None,
    )
    .await;
    assert_eq!(st, 400, "@everyone cannot be deleted");
    step("PASS guard rails");

    // ----- role hierarchy -----
    step("hierarchy: bob (MANAGE_ROLES at a middle position) can act only below his top role");
    let (st, mid) = rest("POST", &alice_token, &format!("/guilds/{guild_id}/roles"), Some(json!({"name": "hier-mid", "permissions": "268435456"}))).await;
    assert!(st / 100 == 2, "create hier-mid: {mid}");
    let mid_id = mid["id"].as_str().unwrap().to_owned();
    let (st, high) = rest("POST", &alice_token, &format!("/guilds/{guild_id}/roles"), Some(json!({"name": "hier-high", "permissions": "0"}))).await;
    assert!(st / 100 == 2, "create hier-high: {high}");
    let high_id = high["id"].as_str().unwrap().to_owned();
    let (st, _) = rest("PUT", &alice_token, &format!("/guilds/{guild_id}/members/{bob_id}/roles/{mid_id}"), None).await;
    assert!(st / 100 == 2, "give bob hier-mid");
    let (st, _) = rest("PATCH", &bob_token, &format!("/guilds/{guild_id}/roles/{high_id}"), Some(json!({"name": "stolen"}))).await;
    assert_eq!(st, 403, "bob must not edit a role above his top role");
    let (st, _) = rest("PUT", &bob_token, &format!("/guilds/{guild_id}/members/{bob_id}/roles/{high_id}"), None).await;
    assert_eq!(st, 403, "bob must not hand out a role above his top role");
    let (st, _) = rest("PUT", &bob_token, &format!("/guilds/{guild_id}/members/{alice_id}/roles/{mid_id}"), None).await;
    assert_eq!(st, 403, "bob must not change the owner");
    step("PASS hierarchy blocks edits, grants and changes to the owner");

    step("hierarchy: a non-owner creates a role that sits at the bottom and can then edit it");
    let (st, own) = rest("POST", &bob_token, &format!("/guilds/{guild_id}/roles"), Some(json!({"name": "bobs-role"}))).await;
    assert!(st / 100 == 2, "bob creates role: {own}");
    let own_id = own["id"].as_str().unwrap().to_owned();
    let (st, _) = rest("PATCH", &bob_token, &format!("/guilds/{guild_id}/roles/{own_id}"), Some(json!({"name": "bobs-renamed"}))).await;
    assert!(st / 100 == 2, "bob edits his own role: {st}");
    step("PASS non-owner role creation and edit below the top");

    // ----- Flint's own role actions (ServerAdmin commands) -----
    use model::server_admin::Action as AdminAction;
    use model::server_roles::{Action as RoleAction, Edit as RoleEdit};
    use model::Patch;
    let empty_edit = |name: Option<String>, permissions: Option<u128>, hoist: Option<bool>| RoleEdit {
        name,
        colors: None,
        permissions,
        permission_mask: if permissions.is_some() { u128::MAX } else { 0 },
        hoist,
        mentionable: None,
        icon: Patch::Absent,
        unicode_emoji: Patch::Absent,
    };
    let find_role = |roles: &Value, name: &str| -> Option<String> {
        roles.as_array().unwrap().iter().find(|r| r["name"] == name).map(|r| r["id"].as_str().unwrap().to_owned())
    };

    step("Flint UI: create a role through its role editor");
    let create = admin(&api_alice, guild_model, AdminAction::Roles(RoleAction::Create(empty_edit(Some("flint-role".into()), Some(0), Some(true)))), 701).await;
    match create {
        Event::ServerAdmin(client_core::server_admin::Event { result: Ok(_), .. }) => {}
        Event::ServerAdmin(client_core::server_admin::Event { result: Err(e), .. }) => panic!("flint create role failed: {e}"),
        _ => panic!("flint create role: unexpected event"),
    }
    let (st, roles_now) = rest("GET", &alice_token, &format!("/guilds/{guild_id}"), None).await;
    assert!(st == 200, "guild after create: {roles_now}");
    let flint_role = find_role(&roles_now["roles"], "flint-role").expect("flint-role exists");
    step("PASS Flint create role");

    step("Flint UI: give bob the role through member roles");
    let bob_model: Id = bob_id.parse().expect("bob id");
    let role_model: Id = flint_role.parse().expect("role id");
    let assign = admin(&api_alice, guild_model, AdminAction::SetRole { user: bob_model, role: role_model, assigned: true }, 702).await;
    assert!(matches!(assign, Event::ServerAdmin(client_core::server_admin::Event { result: Ok(_), .. })), "flint assign role failed");
    let (st, member) = rest("GET", &alice_token, &format!("/guilds/{guild_id}/members/{bob_id}"), None).await;
    assert!(st == 200 && member["roles"].as_array().unwrap().iter().any(|r| r == flint_role.as_str()), "bob holds the role: {member}");
    step("PASS Flint member role assignment");

    step("Flint UI: rename, move, then remove the member role and delete the role");
    let rename = admin(&api_alice, guild_model, AdminAction::Roles(RoleAction::Edit { id: role_model, edit: empty_edit(Some("flint-renamed".into()), None, None) }), 703).await;
    assert!(matches!(rename, Event::ServerAdmin(client_core::server_admin::Event { result: Ok(_), .. })), "flint rename failed");
    let mv = admin(&api_alice, guild_model, AdminAction::Roles(RoleAction::Move { id: role_model, position: 2 }), 704).await;
    assert!(matches!(mv, Event::ServerAdmin(client_core::server_admin::Event { result: Ok(_), .. })), "flint move failed");
    let unassign = admin(&api_alice, guild_model, AdminAction::SetRole { user: bob_model, role: role_model, assigned: false }, 705).await;
    assert!(matches!(unassign, Event::ServerAdmin(client_core::server_admin::Event { result: Ok(_), .. })), "flint unassign failed");
    let delete = admin(&api_alice, guild_model, AdminAction::Roles(RoleAction::Delete(role_model)), 706).await;
    assert!(matches!(delete, Event::ServerAdmin(client_core::server_admin::Event { result: Ok(_), .. })), "flint delete failed");
    let (st, after) = rest("GET", &alice_token, &format!("/guilds/{guild_id}"), None).await;
    assert!(st == 200 && find_role(&after["roles"], "flint-renamed").is_none(), "role gone after delete");
    step("PASS Flint rename, move, unassign and delete");

    // ----- Flint's channel editor: rename, topic and an @everyone overwrite -----
    step("Flint UI: channel editor renames general, sets a topic, and denies @everyone SEND_MESSAGES");
    let channel_edit = |name: &str, topic: &str, overwrites: Vec<model::permissions::Overwrite>| client_core::channel_actions::Edit {
        name: name.into(),
        topic: topic.into(),
        slowmode: 0,
        nsfw: false,
        overwrites,
        forum: None,
    };
    // Like the editor: load the channel first, then edit against what the server reported.
    let loaded = api_alice
        .execute(Command::ChannelAction { guild: guild_model, channel, request: 800, action: client_core::channel_actions::Action::Load })
        .await;
    let before = match loaded {
        Event::ChannelAction(client_core::channel_actions::Event::Finished { result: Ok(client_core::channel_actions::Outcome::Details(edit)), .. }) => edit,
        Event::ChannelAction(client_core::channel_actions::Event::Finished { result: Err(e), .. }) => panic!("flint channel load failed: {e}"),
        _ => panic!("flint channel load: unexpected event"),
    };
    println!("[e2e] loaded before: name={} topic={:?} slowmode={} nsfw={} overwrites={:?}", before.name, before.topic, before.slowmode, before.nsfw, before.overwrites);
    let deny_send = model::permissions::Overwrite { id: guild_model, kind: 0, allow: 0, deny: 2048 };
    let after = channel_edit("e2e-renamed", "edited in flint", vec![deny_send]);
    let edited = api_alice
        .execute(Command::ChannelAction { guild: guild_model, channel, request: 801, action: client_core::channel_actions::Action::Edit { before, after } })
        .await;
    match &edited {
        Event::ChannelAction(client_core::channel_actions::Event::Finished { result: Ok(_), .. }) => {}
        Event::ChannelAction(client_core::channel_actions::Event::Finished { result: Err(e), .. }) => panic!("flint channel edit failed: {e}"),
        _ => panic!("flint channel edit: unexpected event"),
    }
    let (st, edited_json) = rest("GET", &alice_token, &format!("/channels/{general}"), None).await;
    assert!(st == 200 && edited_json["name"] == "e2e-renamed" && edited_json["topic"] == "edited in flint", "renamed: {edited_json}");
    assert!(edited_json["permission_overwrites"].as_array().unwrap().iter().any(|o| o["id"] == guild_id.as_str() && o["deny"] == "2048"), "overwrite saved: {edited_json}");
    step("PASS Flint channel edit saved name, topic and overwrite");

    step("Flint UI: the overwrite Flint saved blocks bob's send");
    let (st, _) = rest("POST", &bob_token, &format!("/channels/{general}/messages"), Some(json!({"content": "should be blocked"}))).await;
    assert_eq!(st, 403, "bob is blocked by the saved overwrite");
    step("PASS overwrite enforced");

    step("ALL CHECKS PASSED");
    std::process::exit(0);
}
