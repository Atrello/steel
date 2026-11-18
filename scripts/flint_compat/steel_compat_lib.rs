#[cfg(test)]
mod tests {
    use discord_protocol::{MessageDto, Ready};

    #[test]
    fn steel_ready_decodes() {
        let json = r#"{"v":10,"user":{"id":"116591259717364818","username":"alice","global_name":null,"avatar":null,"bot":false},
        "session_id":"s1","resume_gateway_url":"/gateway",
        "guilds":[{"id":"200","name":"Spark Club","owner_id":"116591259717364818","channels":[
            {"id":"201","type":0,"guild_id":"200","name":"general","position":0,"topic":null}]}],
        "private_channels":[{"id":"300","type":1,"name":null,"position":0,"topic":null,
            "recipients":[{"id":"301","username":"bob","global_name":null,"avatar":null,"bot":false}]}],
        "read_state":[{"id":"201","last_message_id":"250","mention_count":0}]}"#;
        let ready: Ready = serde_json::from_str(json).expect("READY decodes");
        assert_eq!(ready.guilds.len(), 1);
        assert_eq!(ready.guilds[0].channels.len(), 1);
        assert_eq!(ready.private_channels.len(), 1);
        assert_eq!(ready.private_channels[0].recipients.len(), 1);
        assert_eq!(ready.session_id, "s1");
        assert!(ready.read_state.is_some());
    }

    #[test]
    fn steel_message_decodes() {
        // Captured from a running steeld: a message sent with a staged upload.
        let json = r#"{"id": "116596026010648961", "channel_id": "116596025966349584", "author": {"id": "116596025941122299", "username": "fx024258", "global_name": null, "avatar": null, "bot": false}, "content": "see attached", "timestamp": "2026-11-18T17:50:58.850Z", "edited_timestamp": null, "type": 0, "attachments": [{"id": "4894504491553008349", "filename": "notes.txt", "content_type": null, "size": 13, "url": "/cdn/attachments/116596025966349584/4894504491553008349/notes.txt"}]}"#;
        let msg: MessageDto = serde_json::from_str(json).expect("message decodes");
        assert_eq!(msg.content, "see attached");
        assert_eq!(msg.attachments.0.len(), 1);
        assert_eq!(msg.attachments.0[0].size, 13);
        assert_eq!(msg.kind, 0);
    }
}
