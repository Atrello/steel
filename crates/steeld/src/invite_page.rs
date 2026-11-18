//! `GET /invite/{code}`: a plain HTML page for shared invite links. Flint creates links in this
//! form, so opening one in a browser shows what the server is and how to join it, instead of a 404.

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/invite/{code}", get(invite_page))
}

/// Escape text for HTML element and attribute content.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn page(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         <style>\n\
         body {{ margin: 0; font-family: system-ui, sans-serif; background: #1e1f22; color: #f2f3f5; }}\n\
         main {{ max-width: 32rem; margin: 10vh auto; padding: 0 1rem; }}\n\
         h1 {{ font-size: 1.75rem; margin-bottom: 0.25rem; }}\n\
         p {{ color: #b5bac1; line-height: 1.5; }}\n\
         code {{ background: #2b2d31; padding: 0.15rem 0.4rem; border-radius: 4px; }}\n\
         </style>\n</head>\n<body>\n<main>\n{body}\n</main>\n</body>\n</html>\n"
    );
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

async fn invite_page(State(state): State<AppState>, Path(code): Path<String>) -> Response {
    let found: Result<Option<(String, String, i64)>, sqlx::Error> = async {
        sqlx::query_as(&crate::db::sql(
            "SELECT g.name, i.code, (SELECT COUNT(*) FROM guild_members m WHERE m.guild_id = g.id) \
             FROM invites i JOIN guilds g ON g.id = i.guild_id WHERE i.code = ?",
        ))
        .bind(&code)
        .fetch_optional(&state.db)
        .await
    }
    .await;

    match found {
        Ok(Some((name, code, members))) => {
            let name = escape(&name);
            let code = escape(&code);
            let noun = if members == 1 { "member" } else { "members" };
            let body = format!(
                "<h1>You have been invited to {name}</h1>\n\
                 <p>{members} {noun}</p>\n\
                 <p>To join, open Flint, choose <strong>Join a server</strong>, and paste this \
                 invite link or the code <code>{code}</code>.</p>"
            );
            page(StatusCode::OK, &format!("Join {name} on Steel"), &body)
        }
        Ok(None) => page(
            StatusCode::NOT_FOUND,
            "Invite not found",
            "<h1>This invite is not valid</h1>\n\
             <p>It may have expired or been revoked. Ask the person who sent it for a new one.</p>",
        ),
        Err(_) => page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong",
            "<h1>Something went wrong</h1><p>Try the link again in a moment.</p>",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escapes_markup() {
        assert_eq!(
            escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;"
        );
    }
}
