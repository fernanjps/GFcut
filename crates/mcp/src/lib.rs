pub mod ai;
pub mod ffmpeg;
pub mod protocol;
pub mod server;
pub mod tools;

pub use server::McpServer;

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use gfcut_timeline::Project;
    use serde_json::{Value, json};

    use crate::{
        ffmpeg::{parse_silencedetect_stderr, parse_whisper_json},
        protocol::{CallToolResult, JsonRpcResponse, ToolContent},
        server::McpServer,
    };

    #[test]
    fn mcp_stdio_initialize_and_list_tools_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let project_path = temp.path().join("session.gfcut.json");
        let mut server = McpServer::new(&project_path);

        let input_stream = [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "test-agent", "version": "1.0.0" }
                }
            })
            .to_string(),
            // Notification (no `id`) must not produce a response line
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            })
            .to_string(),
        ]
        .join("\n");

        let mut output_buf = Vec::new();
        server
            .serve(Cursor::new(input_stream), &mut output_buf)
            .expect("stdio loop should succeed");

        let output_str = String::from_utf8(output_buf).unwrap();
        let responses: Vec<JsonRpcResponse> = output_str
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        // Only 2 responses expected (initialize + tools/list; notification is silent)
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].id, json!(1));
        assert!(responses[0].error.is_none());

        assert_eq!(responses[1].id, json!(2));
        let tools = responses[1]
            .result
            .as_ref()
            .unwrap()
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap();

        let names: Vec<&str> = tools
            .iter()
            .map(|t| t.get("name").unwrap().as_str().unwrap())
            .collect();

        assert_eq!(
            names,
            vec![
                "get_timeline",
                "add_clip",
                "split_clip",
                "remove_silence",
                "inspect_media",
                "transcribe_media",
                "export_project",
            ]
        );
    }

    #[test]
    fn mcp_tools_mutate_and_persist_timeline_over_stdio() {
        let temp = tempfile::tempdir().unwrap();
        let project_path = temp.path().join("workflow.gfcut.json");
        let output_video_path = temp.path().join("rendered.mp4");
        let mut server = McpServer::new(&project_path);

        // 1. Call `get_timeline` -> initializes and persists default project
        let get_resp = server
            .handle_raw_message(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/call",
                    "params": {
                        "name": "get_timeline",
                        "arguments": {}
                    }
                })
                .to_string(),
            )
            .unwrap();

        let get_result: CallToolResult =
            serde_json::from_value(get_resp.result.unwrap()).unwrap();
        assert!(get_result.is_error.is_none());
        let ToolContent::Text { text } = &get_result.content[0];
        let initial_project: Project = serde_json::from_str(text).unwrap();
        let video_track_id = initial_project.tracks[0].id;

        // 2. Call `add_clip` to insert a 20s video clip
        let add_resp = server
            .handle_raw_message(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "tools/call",
                    "params": {
                        "name": "add_clip",
                        "arguments": {
                            "track_id": video_track_id,
                            "file_path": "raw_footage.mp4",
                            "start_time": 0.0,
                            "in_point": 0.0,
                            "duration": 20.0
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();

        let add_result: CallToolResult =
            serde_json::from_value(add_resp.result.unwrap()).unwrap();
        assert!(add_result.is_error.is_none());
        let ToolContent::Text { text: add_text } = &add_result.content[0];
        let add_json: Value = serde_json::from_str(add_text).unwrap();
        let clip_id = add_json.get("clip_id").unwrap().as_str().unwrap();

        // 3. Call `split_clip` at 8.0s
        let split_resp = server
            .handle_raw_message(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 12,
                    "method": "tools/call",
                    "params": {
                        "name": "split_clip",
                        "arguments": {
                            "clip_id": clip_id,
                            "timestamp": 8.0
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();

        let split_result: CallToolResult =
            serde_json::from_value(split_resp.result.unwrap()).unwrap();
        assert!(split_result.is_error.is_none());

        // 4. Call `remove_silence` removing [3.0..5.0] (2s) with ripple=true -> 18s total
        let silence_resp = server
            .handle_raw_message(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 13,
                    "method": "tools/call",
                    "params": {
                        "name": "remove_silence",
                        "arguments": {
                            "track_id": video_track_id,
                            "intervals": [{ "start": 3.0, "end": 5.0 }],
                            "ripple": true
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();

        let silence_result: CallToolResult =
            serde_json::from_value(silence_resp.result.unwrap()).unwrap();
        assert!(silence_result.is_error.is_none());

        // Verify disk state was atomically updated
        let persisted = Project::load_from_file(&project_path).unwrap();
        assert!((persisted.duration() - 18.0).abs() < 1e-6);
        assert_eq!(persisted.tracks[0].clips.len(), 3);

        // 5. Call `export_project` (dry-run command generation)
        let export_resp = server
            .handle_raw_message(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 14,
                    "method": "tools/call",
                    "params": {
                        "name": "export_project",
                        "arguments": {
                            "output_path": output_video_path,
                            "execute": false
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();

        let export_result: CallToolResult =
            serde_json::from_value(export_resp.result.unwrap()).unwrap();
        assert!(export_result.is_error.is_none());
        let ToolContent::Text { text: export_text } = &export_result.content[0];
        let export_plan: Value = serde_json::from_str(export_text).unwrap();
        assert_eq!(export_plan.get("clip_count").unwrap().as_u64(), Some(3));
        assert!(
            export_plan
                .get("command_line")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("concat=n=3:v=1:a=1")
        );
    }

    #[test]
    fn parses_ffmpeg_silencedetect_stderr_output() {
        let sample_stderr = "\
[silencedetect @ 0x55f8] silence_start: 1.250
[silencedetect @ 0x55f8] silence_end: 3.750 | silence_duration: 2.500
[silencedetect @ 0x55f8] silence_start: 10.000
[silencedetect @ 0x55f8] silence_end: 12.125 | silence_duration: 2.125
";
        let intervals = parse_silencedetect_stderr(sample_stderr);
        assert_eq!(intervals.len(), 2);
        assert!((intervals[0].start - 1.250).abs() < 1e-6);
        assert!((intervals[0].end - 3.750).abs() < 1e-6);
        assert!((intervals[1].start - 10.000).abs() < 1e-6);
        assert!((intervals[1].end - 12.125).abs() < 1e-6);
    }

    #[test]
    fn mcp_transcribe_media_attaches_transcript_to_timeline_asset() {
        let temp = tempfile::tempdir().unwrap();
        let project_path = temp.path().join("transcribe.gfcut.json");
        let mut server = McpServer::new(&project_path);

        let resp = server
            .handle_raw_message(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 20,
                    "method": "tools/call",
                    "params": {
                        "name": "transcribe_media",
                        "arguments": {
                            "file_path": "interview_take1.mp4",
                            "model": "tiny"
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();

        let call_result: CallToolResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(call_result.is_error.is_none());

        let ToolContent::Text { text } = &call_result.content[0];
        let payload: Value = serde_json::from_str(text).unwrap();

        assert_eq!(
            payload.get("file_path").unwrap().as_str(),
            Some("interview_take1.mp4")
        );
        let readable = payload
            .get("readable_segments")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(!readable.is_empty());

        // Verify that the transcript was persisted atomically inside `.gfcut.json`
        let persisted = Project::load_from_file(&project_path).unwrap();
        assert_eq!(persisted.assets.len(), 1);
        let asset = &persisted.assets[0];
        let transcript = asset
            .transcript
            .as_ref()
            .expect("transcript must be attached to asset");
        assert_eq!(transcript.segments.len(), 3);
        assert!(transcript.segments[0].text.contains("interview_take1.mp4"));
    }

    #[test]
    fn parses_whisper_json_segments_accurately() {
        let raw = r#"{
            "language": "es",
            "segments": [
                { "start": 0.0, "end": 2.4, "text": " Hola a todos " },
                { "start": 2.4, "end": 5.8, "text": " Bienvenidos a GFCut " }
            ]
        }"#;

        let transcript = parse_whisper_json(raw).expect("valid whisper json should parse");
        assert_eq!(transcript.language.as_deref(), Some("es"));
        assert_eq!(transcript.segments.len(), 2);
        assert_eq!(transcript.segments[0].text, "Hola a todos");
        assert!((transcript.segments[1].start - 2.4).abs() < 1e-6);
        assert!((transcript.segments[1].end - 5.8).abs() < 1e-6);
    }
}
