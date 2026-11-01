use serde::Deserialize;
use serde_json::{Value, json};

use super::client::Client;
use super::error::Result;

#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub speech: String,
    pub conversation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Pipeline {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub language: String,
    /// `None` when the assistant has no speech-to-text, so it cannot listen.
    #[serde(default)]
    pub stt_engine: Option<String>,
    #[serde(default)]
    pub tts_engine: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Pipelines {
    pub pipelines: Vec<Pipeline>,
    #[serde(rename = "preferred_pipeline", default)]
    pub preferred: Option<String>,
}

impl Pipelines {
    pub fn get(&self, id: &str) -> Option<&Pipeline> {
        self.pipelines.iter().find(|pipeline| pipeline.id == id)
    }

    /// Falls back to the server's preferred pipeline when `wanted` was deleted.
    pub fn resolve(&self, wanted: Option<&str>) -> Option<&Pipeline> {
        wanted
            .and_then(|id| self.get(id))
            .or_else(|| self.preferred.as_deref().and_then(|id| self.get(id)))
            .or_else(|| self.pipelines.first())
    }
}

/// Setting value that follows the server's preferred pipeline. Pipeline IDs are
/// ULIDs, so a leading `@` cannot collide with one.
pub const PREFERRED: &str = "@preferred";

/// A specific default wins; an empty default means "Last Used". `None` lets the
/// server pick its preferred pipeline.
pub fn wanted_pipeline(default: &str, last_used: &str) -> Option<String> {
    if default == PREFERRED {
        return None;
    }
    [default, last_used]
        .into_iter()
        .find(|id| !id.is_empty())
        .map(str::to_owned)
}

pub async fn pipelines(client: &Client) -> Result<Pipelines> {
    let result = client
        .call(json!({ "type": "assist_pipeline/pipeline/list" }))
        .await?;
    Ok(serde_json::from_value(result)?)
}

/// Starts a typed turn. Its events are the same as a spoken turn's after
/// speech-to-text, so `parse_voice_event` reads them.
pub async fn ask(
    client: &Client,
    text: &str,
    pipeline: Option<&str>,
    conversation_id: Option<&str>,
    speak_reply: bool,
) -> Result<super::client::Subscription> {
    let mut request = json!({
        "type": "assist_pipeline/run",
        "start_stage": "intent",
        "end_stage": if speak_reply { "tts" } else { "intent" },
        "input": { "text": text },
    });
    if let Some(id) = pipeline {
        request["pipeline"] = json!(id);
    }
    if let Some(id) = conversation_id {
        request["conversation_id"] = json!(id);
    }
    client.subscribe(request).await
}

/// Home Assistant expects 16 kHz, 16-bit mono PCM from WebSocket clients.
pub const SAMPLE_RATE: u32 = 16_000;

/// Starts a spoken turn. Audio goes in as binary frames prefixed with the
/// handler ID from `Started`; a frame holding only that byte ends the audio.
pub async fn listen(
    client: &Client,
    pipeline: Option<&str>,
    conversation_id: Option<&str>,
    speak_reply: bool,
) -> Result<super::client::Subscription> {
    let mut request = json!({
        "type": "assist_pipeline/run",
        "start_stage": "stt",
        "end_stage": if speak_reply { "tts" } else { "intent" },
        "input": { "sample_rate": SAMPLE_RATE },
    });
    if let Some(id) = pipeline {
        request["pipeline"] = json!(id);
    }
    if let Some(id) = conversation_id {
        request["conversation_id"] = json!(id);
    }
    client.subscribe(request).await
}

#[derive(Clone, Debug, PartialEq)]
pub enum VoiceEvent {
    Started {
        handler: u8,
    },
    /// The server heard speech end; no more audio is needed.
    SpeechEnded,
    Heard(String),
    Replied(Reply),
    /// A path such as `/api/tts_proxy/…`, or a full URL.
    Audio(String),
    Failed {
        code: String,
        message: String,
    },
    Finished,
}

pub fn parse_voice_event(event: &Value) -> Option<VoiceEvent> {
    let data = &event["data"];
    Some(match event["type"].as_str()? {
        "run-start" => VoiceEvent::Started {
            handler: data["runner_data"]["stt_binary_handler_id"]
                .as_u64()
                .and_then(|id| u8::try_from(id).ok())?,
        },
        "stt-vad-end" => VoiceEvent::SpeechEnded,
        "stt-end" => VoiceEvent::Heard(
            data["stt_output"]["text"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_owned(),
        ),
        "intent-end" => VoiceEvent::Replied(parse_intent_end(data)),
        "tts-end" => VoiceEvent::Audio(data["tts_output"]["url"].as_str()?.to_owned()),
        "error" => VoiceEvent::Failed {
            code: data["code"].as_str().unwrap_or("error").to_owned(),
            message: data["message"].as_str().unwrap_or_default().to_owned(),
        },
        "run-end" => VoiceEvent::Finished,
        _ => return None,
    })
}

fn parse_intent_end(data: &Value) -> Reply {
    let output = &data["intent_output"];
    let speech = output["response"]["speech"]["plain"]["speech"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    Reply {
        speech,
        conversation_id: output["conversation_id"].as_str().map(str::to_owned),
    }
}

/// Lowercase words without punctuation, with contractions spelled out, since
/// speech-to-text writes them either way.
fn normalized(text: &str) -> String {
    let words = text
        .to_lowercase()
        .replace('’', "'")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>();
    let mut out = Vec::new();
    for word in words.split_whitespace() {
        let expanded = [
            ("'s", " is"),
            ("'re", " are"),
            ("'m", " am"),
            ("'ll", " will"),
        ]
        .iter()
        .find_map(|(short, long)| word.strip_suffix(short).map(|stem| format!("{stem}{long}")))
        .unwrap_or_else(|| word.to_owned());
        out.push(expanded);
    }
    out.join(" ").replace("good bye", "goodbye")
}

/// True when everything said is made of dismissal phrases, in any order or
/// combination, such as "that's all, goodbye".
pub fn is_farewell(text: &str, phrases: &[impl AsRef<str>]) -> bool {
    let said = normalized(text);
    let words: Vec<&str> = said.split(' ').filter(|word| !word.is_empty()).collect();
    let phrases: Vec<String> = phrases.iter().map(|p| normalized(p.as_ref())).collect();
    let phrases: Vec<Vec<&str>> = phrases
        .iter()
        .filter(|phrase| !phrase.is_empty())
        .map(|phrase| phrase.split(' ').collect())
        .collect();
    // reachable[i] is true when the first i words are all dismissal phrases.
    let mut reachable = vec![false; words.len() + 1];
    reachable[0] = true;
    for start in 0..words.len() {
        if !reachable[start] {
            continue;
        }
        for phrase in &phrases {
            if words[start..].starts_with(phrase) {
                reachable[start + phrase.len()] = true;
            }
        }
    }
    !words.is_empty() && reachable[words.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_farewells() {
        let phrases = [
            "goodbye",
            "thank you",
            "thanks",
            "stop",
            "You're dismissed",
            "That’s all",
            "that's it",
        ];
        assert!(is_farewell("you are dismissed, good bye", &phrases));
        assert!(is_farewell("That is it", &phrases));
        let farewell = |text| is_farewell(text, &phrases);
        assert!(farewell("Goodbye."));
        assert!(farewell("That's all, goodbye!"));
        assert!(farewell("Thanks, goodbye"));
        assert!(farewell("Thank you. That’s it."));
        assert!(farewell("you're DISMISSED"));
        assert!(farewell("Stop"));
        assert!(!farewell("Turn off the lights, goodbye"));
        assert!(!farewell("Stop the music"));
        assert!(!farewell("Bye"));
        assert!(!farewell(""));
        assert!(!is_farewell("goodbye", &[" ", ""]));
    }

    #[test]
    fn voice_events() {
        let start = json!({ "type": "run-start", "data": { "runner_data": { "stt_binary_handler_id": 1, "timeout": 300 } } });
        assert_eq!(
            parse_voice_event(&start),
            Some(VoiceEvent::Started { handler: 1 })
        );
        let heard = json!({ "type": "stt-end", "data": { "stt_output": { "text": " turn on the lamp " } } });
        assert_eq!(
            parse_voice_event(&heard),
            Some(VoiceEvent::Heard("turn on the lamp".into()))
        );
        let audio = json!({ "type": "tts-end", "data": { "tts_output": { "url": "/api/tts_proxy/x.mp3", "mime_type": "audio/mpeg" } } });
        assert_eq!(
            parse_voice_event(&audio),
            Some(VoiceEvent::Audio("/api/tts_proxy/x.mp3".into()))
        );
        assert_eq!(
            parse_voice_event(&json!({ "type": "stt-vad-start", "data": {} })),
            None
        );
    }

    fn sample() -> Pipelines {
        serde_json::from_value(json!({
            "pipelines": [
                { "id": "p1", "name": "Home Assistant", "language": "en", "stt_engine": null },
                { "id": "p2", "name": "Ollama", "language": "en" }
            ],
            "preferred_pipeline": "p2"
        }))
        .unwrap()
    }

    #[test]
    fn reads_speech_and_conversation() {
        let data = json!({
            "intent_output": {
                "conversation_id": "01J",
                "response": { "speech": { "plain": { "speech": "Turned on the light" } } }
            }
        });
        assert_eq!(
            parse_intent_end(&data),
            Reply {
                speech: "Turned on the light".into(),
                conversation_id: Some("01J".into()),
            }
        );
    }

    #[test]
    fn parses_pipeline_list() {
        let pipelines = sample();
        assert_eq!(pipelines.pipelines.len(), 2);
        assert_eq!(pipelines.preferred.as_deref(), Some("p2"));
    }

    #[test]
    fn resolve_falls_back_to_preferred_then_first() {
        let mut pipelines = sample();
        assert_eq!(pipelines.resolve(Some("p1")).unwrap().id, "p1");
        assert_eq!(pipelines.resolve(Some("deleted")).unwrap().id, "p2");
        assert_eq!(pipelines.resolve(None).unwrap().id, "p2");
        pipelines.preferred = None;
        assert_eq!(pipelines.resolve(None).unwrap().id, "p1");
        assert!(Pipelines::default().resolve(None).is_none());
    }

    #[test]
    fn default_overrides_last_used() {
        assert_eq!(wanted_pipeline("p1", "p2").as_deref(), Some("p1"));
        assert_eq!(wanted_pipeline("", "p2").as_deref(), Some("p2"));
        assert_eq!(wanted_pipeline("", ""), None);
        assert_eq!(wanted_pipeline(PREFERRED, "p2"), None);
    }
}
