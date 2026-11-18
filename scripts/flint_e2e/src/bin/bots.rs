//! Bot check: Flint's client code drives a command, a component press and an ephemeral reply
//! against steeld, with the example echo bot answering. Run steeld on 127.0.0.1:8080 first.
use client_core::{
    Command, Event,
    auth::{Failure, SessionSecret},
    interactions::{Data, Event as InteractionEvent, Request},
};
use discord_api::DiscordApi;
use model::{
    Id,
    application_commands::{Argument, Command as AppCommand, Invocation, Value},
};
use serde_json::{Value as Json, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, watch};

const SERVER: &str = "http://127.0.0.1:8080";
const API: &str = "http://127.0.0.1:8080/api/v10";
const WAIT: Duration = Duration::from_secs(20);
const BOT_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/echo-bot/echo_bot.py"
);

type Rx = mpsc::UnboundedReceiver<Event>;

fn step(msg: &str) {
    println!("[bots] {msg}");
}

async fn rest(method: &str, token: &str, path: &str, body: Option<Json>) -> (u16, Json) {
    let client = wreq::Client::new();
    let url = format!("{API}{path}");
    let mut req = match method {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        other => panic!("method {other}"),
    };
    req = req.header("authorization", token);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await.expect("rest send");
    let status = resp.status().as_u16();
    let text = resp.text().await.expect("rest body");
    (
        status,
        serde_json::from_str(&text).unwrap_or(Json::String(text)),
    )
}

fn start_gateway(secret: Arc<SessionSecret>) -> Rx {
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Event>();
    let (_sub_tx, sub_rx) = watch::channel::<Option<discord_gateway::MemberSubscription>>(None);
    let url = model::endpoints::get().gateway_url.clone();
    let emit = move |event: Event| -> Result<(), Failure> {
        ev_tx.send(event).map_err(|_| Failure::Protocol)
    };
    tokio::spawn(async move { discord_gateway::run(secret, url, sub_rx, emit).await });
    ev_rx
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

fn invocation(command: AppCommand, options: Vec<Argument>) -> Box<Invocation> {
    Box::new(Invocation { command, options })
}

#[tokio::main]
async fn main() {
    model::endpoints::configure(SERVER).expect("configure server");
    let tag = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 100_000_000;
    let password = "correct-horse-battery-1";

    step("register owner and member through Flint's discord_api::auth");
    let owner = Arc::new(
        discord_api::auth::register(&format!("botown{tag}"), password, None)
            .await
            .expect("register owner"),
    );
    let member = Arc::new(
        discord_api::auth::register(&format!("botmem{tag}"), password, None)
            .await
            .expect("register member"),
    );
    let (owner_token, member_token) = (owner.expose().to_owned(), member.expose().to_owned());

    let (st, guild) = rest(
        "POST",
        &owner_token,
        "/guilds",
        Some(json!({"name": "Bot guild"})),
    )
    .await;
    assert!(st / 100 == 2, "create guild: {guild}");
    let guild_id = guild["id"].as_str().unwrap().to_owned();
    let channel_id = guild["channels"][0]["id"].as_str().unwrap().to_owned();
    let (_, invite) = rest(
        "POST",
        &owner_token,
        &format!("/channels/{channel_id}/invites"),
        Some(json!({})),
    )
    .await;
    let (st, _) = rest(
        "POST",
        &member_token,
        &format!("/invites/{}", invite["code"].as_str().unwrap()),
        None,
    )
    .await;
    assert!(st / 100 == 2, "member joins");

    step("owner creates an application and adds its bot to the guild");
    let (st, app) = rest(
        "POST",
        &owner_token,
        "/applications",
        Some(json!({"name": "Echo Bot"})),
    )
    .await;
    assert!(st == 201, "create application: {app}");
    let app_id = app["id"].as_str().unwrap().to_owned();
    let bot_id = app["bot"]["id"].as_str().unwrap().to_owned();
    let bot_token = app["token"].as_str().unwrap().to_owned();
    let (st, _) = rest(
        "PUT",
        &owner_token,
        &format!("/applications/{app_id}/guilds/{guild_id}"),
        None,
    )
    .await;
    assert!(st == 204, "add bot to guild: {st}");

    step("start the example echo bot (python3 examples/echo-bot/echo_bot.py)");
    let mut bot = std::process::Command::new("python3")
        .arg(BOT_SCRIPT)
        .env("STEEL_URL", SERVER)
        .env("APPLICATION_ID", &app_id)
        .env("BOT_TOKEN", &bot_token)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start echo bot");

    let result = run_checks(
        &member,
        &member_token,
        &guild_id,
        &channel_id,
        &app_id,
        &bot_id,
    )
    .await;
    let _ = bot.kill();
    let _ = bot.wait();
    match result {
        Ok(()) => println!("[bots] ALL CHECKS PASSED"),
        Err(e) => {
            println!("[bots] FAILED: {e}");
            std::process::exit(1);
        }
    }
}

async fn run_checks(
    member: &Arc<SessionSecret>,
    member_token: &str,
    guild_id: &str,
    channel_id: &str,
    app_id: &str,
    bot_id: &str,
) -> Result<(), String> {
    let api = DiscordApi::new(member.clone()).map_err(|e| format!("api: {e:?}"))?;
    let mut rx = start_gateway(member.clone());
    // The client submits interactions with the gateway session id it received at READY.
    let mut session = None;
    let mut ready = false;
    tokio::time::timeout(WAIT, async {
        while !(ready && session.is_some()) {
            match rx.recv().await.expect("gateway channel closed") {
                Event::Startup(_) => ready = true,
                Event::Interaction(InteractionEvent::Session(s)) => session = Some(s),
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for READY and session".to_string())?;
    api.interaction_session(session)
        .map_err(|e| format!("session: {e:?}"))?;

    let guild: Id = guild_id.parse().unwrap();
    let channel: Id = channel_id.parse().unwrap();
    let app: Id = app_id.parse().unwrap();

    step("Flint loads the application command index until the bot's commands appear");
    let commands = wait_until_commands(&api, channel, guild, &["ping", "echo", "hello"]).await?;
    step(&format!(
        "PASS index: {:?}",
        commands.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
    ));
    let by_name = |name: &str| commands.iter().find(|c| c.name == name).cloned().unwrap();

    step("member runs /ping through Flint; the bot replies, bot-authored message arrives");
    submit(
        &api,
        Request {
            request: 1,
            nonce: "7001".into(),
            application_id: app,
            channel_id: channel,
            guild_id: Some(guild),
            message_id: None,
            message_flags: 0,
            data: Data::ApplicationCommand {
                invocation: invocation(by_name("ping"), vec![]),
            },
        },
    )
    .await?;
    let pong = wait_for(&mut rx, "pong", |e| match e {
        Event::Message(m) if m.content == "pong" => Some(m.author.kind == model::AccountKind::Bot),
        _ => None,
    })
    .await;
    if !pong {
        return Err("pong author is not a bot".into());
    }
    step("PASS /ping -> pong from bot");

    step("member runs /echo; the reply is ephemeral");
    let text = Argument {
        kind: 3,
        name: "text".into(),
        value: Some(Value::String("flint says hi".into())),
        options: vec![],
    };
    submit(
        &api,
        Request {
            request: 2,
            nonce: "7002".into(),
            application_id: app,
            channel_id: channel,
            guild_id: Some(guild),
            message_id: None,
            message_flags: 0,
            data: Data::ApplicationCommand {
                invocation: invocation(by_name("echo"), vec![text]),
            },
        },
    )
    .await?;
    // Flint delivers ephemeral messages as their own interaction event, not as a channel message.
    wait_for(&mut rx, "ephemeral echo", |e| match e {
        Event::Interaction(InteractionEvent::Ephemeral(m))
            if m.content == "flint says hi" && m.ephemeral =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    step("PASS /echo ephemeral reply");

    step("member runs /hello and gets a message with a button");
    submit(
        &api,
        Request {
            request: 3,
            nonce: "7003".into(),
            application_id: app,
            channel_id: channel,
            guild_id: Some(guild),
            message_id: None,
            message_flags: 0,
            data: Data::ApplicationCommand {
                invocation: invocation(by_name("hello"), vec![]),
            },
        },
    )
    .await?;
    let hello = wait_for(&mut rx, "hello message", |e| match e {
        Event::Message(m) if m.content == "Hello! Press the button." => Some(m.clone()),
        _ => None,
    })
    .await;
    step("PASS /hello message posted");

    step("member presses the button; the bot edits the message");
    submit(
        &api,
        Request {
            request: 4,
            nonce: "7004".into(),
            application_id: app,
            channel_id: channel,
            guild_id: Some(guild),
            message_id: Some(hello.id),
            message_flags: hello.flags,
            data: Data::Component {
                custom_id: "count:0".into(),
                component_type: 2,
                values: vec![],
            },
        },
    )
    .await?;
    wait_for(&mut rx, "message update", |e| match e {
        Event::Patch(p) if p.id == hello.id => Some(()),
        _ => None,
    })
    .await;
    step("PASS button press -> MESSAGE_UPDATE");

    step("member opens a DM with the bot and runs /ping there");
    let (st, dm) = rest(
        "POST",
        member_token,
        "/users/@me/channels",
        Some(json!({"recipient_id": bot_id})),
    )
    .await;
    if st / 100 != 2 {
        return Err(format!("open DM: {st} {dm}"));
    }
    let dm: Id = dm["id"].as_str().unwrap().parse().unwrap();
    let dm_commands = wait_until_dm_commands(&api, dm, "ping").await?;
    let dm_ping = dm_commands
        .iter()
        .find(|c| c.name == "ping")
        .cloned()
        .unwrap();
    submit(
        &api,
        Request {
            request: 5,
            nonce: "7005".into(),
            application_id: app,
            channel_id: dm,
            guild_id: None,
            message_id: None,
            message_flags: 0,
            data: Data::ApplicationCommand {
                invocation: invocation(dm_ping, vec![]),
            },
        },
    )
    .await?;
    wait_for(&mut rx, "DM pong", |e| match e {
        Event::Message(m) if m.content == "pong" && m.channel == dm => Some(()),
        _ => None,
    })
    .await;
    step("PASS DM index and /ping in a bot DM");

    step("member types 'he' into /echo's autocompleted text; the bot's suggestions reach Flint");
    let echo = by_name("echo");
    let typed = [("text".to_string(), "he".to_string())];
    let invocation = echo
        .autocomplete_invocation(&[], &typed, "text")
        .map_err(|e| format!("autocomplete invocation: {e}"))?;
    let nonce = "7006".to_string();
    submit(
        &api,
        Request {
            request: 6,
            nonce: nonce.clone(),
            application_id: app,
            channel_id: channel,
            guild_id: Some(guild),
            message_id: None,
            message_flags: 0,
            data: Data::Autocomplete {
                invocation: Box::new(invocation),
                focused: "text".into(),
            },
        },
    )
    .await?;
    let choices = wait_for(&mut rx, "autocomplete choices", |e| match e {
        Event::Interaction(InteractionEvent::Autocomplete { nonce: n, choices })
            if *n == nonce =>
        {
            Some(choices.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
        }
        _ => None,
    })
    .await;
    if choices.is_empty() || !choices.iter().all(|name| name.starts_with("he")) {
        return Err(format!("unexpected suggestions for 'he': {choices:?}"));
    }
    step(&format!("PASS autocomplete suggestions for 'he': {choices:?}"));

    step("member installs the bot's application from its profile; the installed list shows it, then removing it clears it");
    let bot: Id = bot_id.parse().unwrap();
    for install in [true, false] {
        let event = api
            .execute(Command::AppInstall {
                request: 8,
                bot,
                install,
            })
            .await;
        match event {
            Event::AppInstall { result: Ok(()), .. } => {}
            _ => return Err(format!("install {install}: unexpected event")),
        }
        let listed = match api.execute(Command::AppInstalls { request: 9 }).await {
            Event::AppInstalls { result: Ok(ids), .. } => ids.contains(&bot),
            _ => return Err(format!("list after install {install}: unexpected event")),
        };
        if listed != install {
            return Err(format!("installed list says {listed}, expected {install}"));
        }
    }
    step("PASS install and remove from a bot profile");
    Ok(())
}

async fn submit(api: &DiscordApi, request: Request) -> Result<(), String> {
    let nonce = request.nonce.clone();
    let event = api.execute(Command::Interaction(request)).await;
    match event {
        Event::Interaction(InteractionEvent::Submitted { result: Ok(()), .. }) => Ok(()),
        Event::Interaction(InteractionEvent::Submitted { result: Err(e), .. }) => {
            Err(format!("submit {nonce}: {e:?}"))
        }
        _ => Err(format!("submit {nonce}: unexpected event")),
    }
}

async fn wait_until_commands(
    api: &DiscordApi,
    channel: Id,
    guild: Id,
    want: &[&str],
) -> Result<Vec<AppCommand>, String> {
    for _ in 0..40 {
        let event = api
            .execute(Command::ApplicationCommands {
                channel,
                guild: Some(guild),
                request: 1,
            })
            .await;
        if let Event::ApplicationCommands {
            result: Ok(commands),
            ..
        } = event
        {
            if want
                .iter()
                .all(|name| commands.iter().any(|c| c.name == *name))
            {
                return Ok(commands);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("commands never appeared in Flint's index".into())
}

async fn wait_until_dm_commands(
    api: &DiscordApi,
    channel: Id,
    want: &str,
) -> Result<Vec<AppCommand>, String> {
    for _ in 0..40 {
        let event = api
            .execute(Command::ApplicationCommands {
                channel,
                guild: None,
                request: 2,
            })
            .await;
        if let Event::ApplicationCommands {
            result: Ok(commands),
            ..
        } = event
        {
            if commands.iter().any(|c| c.name == want) {
                return Ok(commands);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("DM commands never appeared in Flint's index".into())
}
