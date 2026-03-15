use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

const DEFAULT_URL: &str = "https://openspeech.bytedance.com/api/v3/tts/unidirectional";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Doubao TTS client
pub struct DoubaoStreamClient {
    app_id: String,
    access_key: String,
    resource_id: String,
    speaker: String,
    api_url: String,
}

impl DoubaoStreamClient {
    pub fn new(
        app_id: String,
        access_key: String,
        resource_id: String,
        speaker: String,
    ) -> Self {
        Self {
            app_id,
            access_key,
            resource_id,
            speaker,
            api_url: DEFAULT_URL.to_string(),
        }
    }

    fn build_payload(
        &self,
        text: &str,
        format: &str,
        sample_rate: u32,
        speed: f32,
        context_texts: Option<Vec<String>>,
        emotion: Option<String>,
    ) -> Value {
        let additions = json!({
            "explicit_language": "zh",
            "disable_markdown_filter": true,
        });

        let mut audio_params = json!({
            "format": format,
            "sample_rate": sample_rate,
            "enable_timestamp": false,
            "speed": speed,
        });

        if let Some(ref emo) = emotion {
            audio_params["emotion"] = json!(emo);
        }

        let mut req_params = json!({
            "text": text,
            "speaker": self.speaker,
            "audio_params": audio_params,
            "additions": additions.to_string(),
        });

        if self.resource_id == "seed-tts-2.0" {
            if let Some(ref ctx) = context_texts {
                if !ctx.is_empty() {
                    req_params["context_texts"] = json!(ctx);
                }
            }
        }

        if self.resource_id == "seed-tts-1.0" {
            req_params["model"] = json!("seed-tts-1.1");
        }

        json!({
            "user": { "uid": "open-xiaoai-bridge" },
            "req_params": req_params,
        })
    }

    /// Parse one JSON line from the API response.
    /// Returns Some(audio_bytes) for data, None for final marker.
    fn parse_line(line: &str) -> Result<Option<Vec<u8>>, String> {
        let data: Value = serde_json::from_str(line)
            .map_err(|e| format!("JSON parse error: {}", e))?;

        let code = data.get("code").and_then(|v| v.as_i64()).unwrap_or(0);

        if code == 0 {
            if let Some(b64_data) = data.get("data").and_then(|v| v.as_str()) {
                if !b64_data.is_empty() {
                    let audio_bytes = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        b64_data,
                    )
                    .map_err(|e| format!("Base64 decode error: {}", e))?;
                    return Ok(Some(audio_bytes));
                }
            }
            Ok(Some(Vec::new()))
        } else if code == 20000000 {
            Ok(None)
        } else {
            let msg = data.get("message").and_then(|v| v.as_str()).unwrap_or("unknown");
            Err(format!("API error code {}: {}", code, msg))
        }
    }

    /// Stream audio chunks via channel (for streaming playback).
    pub async fn stream_audio(
        &self,
        text: &str,
        format: &str,
        sample_rate: u32,
        speed: f32,
        context_texts: Option<Vec<String>>,
        emotion: Option<String>,
        tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<(), String> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("Client build error: {}", e))?;

        let payload = self.build_payload(text, format, sample_rate, speed, context_texts, emotion);

        let response = client
            .post(&self.api_url)
            .header("X-Api-App-Id", &self.app_id)
            .header("X-Api-Access-Key", &self.access_key)
            .header("X-Api-Resource-Id", &self.resource_id)
            .header("Content-Type", "application/json")
            .header("Connection", "keep-alive")
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("API returned status {}: {}", status, body));
        }

        let byte_stream = response.bytes_stream();
        let stream_reader = tokio_util::io::StreamReader::new(
            byte_stream.map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))),
        );
        let mut lines = tokio::io::BufReader::new(stream_reader).lines();

        while let Some(line) = lines.next_line().await.map_err(|e| format!("Read line error: {}", e))? {
            if line.is_empty() {
                continue;
            }
            match Self::parse_line(&line)? {
                Some(bytes) if !bytes.is_empty() => {
                    if tx.send(bytes).await.is_err() {
                        return Ok(());
                    }
                }
                Some(_) => continue,
                None => break,
            }
        }

        Ok(())
    }

    /// Fetch all audio data at once (for non-streaming playback).
    pub async fn fetch_audio(
        &self,
        text: &str,
        format: &str,
        sample_rate: u32,
        speed: f32,
        context_texts: Option<Vec<String>>,
        emotion: Option<String>,
    ) -> Result<Vec<u8>, String> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("Client build error: {}", e))?;

        let payload = self.build_payload(text, format, sample_rate, speed, context_texts, emotion);

        let response = client
            .post(&self.api_url)
            .header("X-Api-App-Id", &self.app_id)
            .header("X-Api-Access-Key", &self.access_key)
            .header("X-Api-Resource-Id", &self.resource_id)
            .header("Content-Type", "application/json")
            .header("Connection", "keep-alive")
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("API returned status {}: {}", status, body));
        }

        let byte_stream = response.bytes_stream();
        let stream_reader = tokio_util::io::StreamReader::new(
            byte_stream.map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))),
        );
        let mut lines = tokio::io::BufReader::new(stream_reader).lines();
        let mut all_data = Vec::new();

        while let Some(line) = lines.next_line().await.map_err(|e| format!("Read line error: {}", e))? {
            if line.is_empty() {
                continue;
            }
            match Self::parse_line(&line)? {
                Some(bytes) if !bytes.is_empty() => {
                    all_data.extend_from_slice(&bytes);
                }
                Some(_) => continue,
                None => break,
            }
        }

        if all_data.is_empty() {
            return Err("TTS synthesis returned empty audio".to_string());
        }

        Ok(all_data)
    }
}
