pub mod decoder;
pub mod doubao;

use crate::tts::decoder::{decode_audio_to_pcm, StreamingDecoder};
use crate::tts::doubao::DoubaoStreamClient;
use open_xiaoai::services::connect::message::MessageManager;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde_json::json;
use std::collections::VecDeque;
use std::time::Instant;
use tokio::sync::mpsc;

const STREAM_BUFFER_THRESHOLD: usize = 8192;
const PLAY_CHUNK_SIZE: usize = 1024 * 1024; // 1MB chunks for WebSocket
const PCM_START_BUFFER_MS: u32 = 240;
const PCM_STREAM_CHUNK_MS: u32 = 60;

/// Send PCM data to device, auto-chunking if larger than PLAY_CHUNK_SIZE.
async fn send_pcm(pcm: Vec<u8>) {
    if pcm.len() <= PLAY_CHUNK_SIZE {
        let _ = MessageManager::instance()
            .send_stream("play", pcm, None)
            .await;
    } else {
        let mut offset = 0;
        while offset < pcm.len() {
            let end = (offset + PLAY_CHUNK_SIZE).min(pcm.len());
            let _ = MessageManager::instance()
                .send_stream("play", pcm[offset..end].to_vec(), None)
                .await;
            offset = end;
        }
    }
}

struct PcmPlaybackBuffer {
    queue: VecDeque<u8>,
    started: bool,
    startup_bytes: usize,
    chunk_bytes: usize,
}

impl PcmPlaybackBuffer {
    fn new(sample_rate: u32) -> Self {
        let bytes_per_second = sample_rate as usize * 2;
        let startup_bytes = bytes_per_second * PCM_START_BUFFER_MS as usize / 1000;
        let chunk_bytes = bytes_per_second * PCM_STREAM_CHUNK_MS as usize / 1000;

        Self {
            queue: VecDeque::new(),
            started: false,
            startup_bytes: startup_bytes.max(chunk_bytes.max(1)),
            chunk_bytes: chunk_bytes.max(1),
        }
    }

    fn push(&mut self, pcm: &[u8]) {
        self.queue.extend(pcm.iter().copied());
    }

    fn drain_ready_chunks(&mut self) -> Vec<Vec<u8>> {
        if !self.started {
            if self.queue.len() < self.startup_bytes {
                return Vec::new();
            }
            self.started = true;
        }

        let mut chunks = Vec::new();
        while self.queue.len() >= self.chunk_bytes {
            let chunk: Vec<u8> = self.queue.drain(..self.chunk_bytes).collect();
            chunks.push(chunk);
        }
        chunks
    }

    fn drain_remaining(&mut self) -> Vec<Vec<u8>> {
        if !self.started && !self.queue.is_empty() {
            self.started = true;
        }

        let mut chunks = self.drain_ready_chunks();
        if !self.queue.is_empty() {
            chunks.push(self.queue.drain(..).collect());
        }
        chunks
    }
}

/// Stream TTS: fetch audio from Doubao API, decode to PCM in chunks, and play via WebSocket.
/// Supports MP3, OGG Vorbis, WAV, FLAC formats.
#[pyfunction]
#[pyo3(signature = (text, app_id, access_key, resource_id, speaker, speed=1.0, format="mp3".to_string(), sample_rate=24000, emotion=None, context_texts=None))]
pub fn tts_stream_play(
    py: Python<'_>,
    text: String,
    app_id: String,
    access_key: String,
    resource_id: String,
    speaker: String,
    speed: f32,
    format: String,
    sample_rate: u32,
    emotion: Option<String>,
    context_texts: Option<Vec<String>>,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let started_at = Instant::now();
        let is_pcm_passthrough = format == "pcm";
        let client = DoubaoStreamClient::new(app_id, access_key, resource_id, speaker);

        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(16);

        let fetch_handle = tokio::spawn({
            let text = text.clone();
            let format = format.clone();
            async move {
                client
                    .stream_audio(&text, &format, sample_rate, speed, context_texts, emotion, tx)
                    .await
            }
        });

        let mut decoder = StreamingDecoder::new(&format, sample_rate);
        let mut playback_buffer = PcmPlaybackBuffer::new(sample_rate);
        let mut accumulated_size: usize = 0;
        let mut first_audio_chunk_ms: Option<u128> = None;
        let mut playback_started_ms: Option<u128> = None;
        let mut total_encoded_bytes: usize = 0;
        let mut total_pcm_bytes: usize = 0;

        while let Some(chunk) = rx.recv().await {
            if first_audio_chunk_ms.is_none() {
                first_audio_chunk_ms = Some(started_at.elapsed().as_millis());
            }
            total_encoded_bytes += chunk.len();

            if is_pcm_passthrough {
                playback_buffer.push(&chunk);
                total_pcm_bytes += chunk.len();
                for pcm_chunk in playback_buffer.drain_ready_chunks() {
                    if playback_started_ms.is_none() {
                        playback_started_ms = Some(started_at.elapsed().as_millis());
                    }
                    send_pcm(pcm_chunk).await;
                }
                continue;
            }

            accumulated_size += chunk.len();
            decoder.feed(&chunk);

            if accumulated_size >= STREAM_BUFFER_THRESHOLD {
                match decoder.decode_all() {
                    Ok(pcm) if !pcm.is_empty() => {
                        total_pcm_bytes += pcm.len();
                        playback_buffer.push(&pcm);
                        for pcm_chunk in playback_buffer.drain_ready_chunks() {
                            if playback_started_ms.is_none() {
                                playback_started_ms = Some(started_at.elapsed().as_millis());
                            }
                            send_pcm(pcm_chunk).await;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        crate::pylog!("[TTS] Decode error (continuing): {}", e);
                    }
                }
                accumulated_size = 0;
            }
        }

        match decoder.decode_all() {
            Ok(pcm) if !pcm.is_empty() => {
                if !is_pcm_passthrough {
                    playback_buffer.push(&pcm);
                }
            }
            Ok(_) => {}
            Err(e) => {
                crate::pylog!("[TTS] Final decode error: {}", e);
            }
        }

        for pcm_chunk in playback_buffer.drain_remaining() {
            if playback_started_ms.is_none() {
                playback_started_ms = Some(started_at.elapsed().as_millis());
            }
            send_pcm(pcm_chunk).await;
        }

        let fetch_failed = match fetch_handle.await {
            Ok(Err(e)) => {
                crate::pylog!("[TTS] Stream fetch error: {}", e);
                Some(e.to_string())
            }
            Err(e) => {
                crate::pylog!("[TTS] Stream fetch task panicked: {}", e);
                Some(e.to_string())
            }
            _ => None,
        };

        let total_ms = started_at.elapsed().as_millis();
        let playback_duration_ms = total_pcm_bytes as u128 * 1000 / (sample_rate as u128 * 2);
        let remaining_ms = playback_duration_ms.saturating_sub(total_ms);

        crate::pylog!(
            "[TTS] Stream summary: format={}, first_chunk={} ms, playback_start={} ms, total={} ms, encoded={} bytes, pcm={} bytes, remaining={} ms",
            format,
            first_audio_chunk_ms.unwrap_or(0),
            playback_started_ms.unwrap_or(0),
            total_ms,
            total_encoded_bytes,
            total_pcm_bytes,
            remaining_ms,
        );

        if let Some(err_msg) = fetch_failed {
            if total_pcm_bytes == 0 {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "TTS stream fetch failed with no audio produced: {}",
                    err_msg
                )));
            }
        }

        if remaining_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(remaining_ms as u64)).await;
        }

        Ok(())
    })
}

/// Non-streaming TTS: fetch all audio, decode to PCM, then play.
#[pyfunction]
#[pyo3(signature = (text, app_id, access_key, resource_id, speaker, speed=1.0, format="mp3".to_string(), sample_rate=24000, emotion=None, context_texts=None))]
pub fn tts_play(
    py: Python<'_>,
    text: String,
    app_id: String,
    access_key: String,
    resource_id: String,
    speaker: String,
    speed: f32,
    format: String,
    sample_rate: u32,
    emotion: Option<String>,
    context_texts: Option<Vec<String>>,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let started_at = Instant::now();
        let client = DoubaoStreamClient::new(app_id, access_key, resource_id, speaker);

        let encoded_audio = client
            .fetch_audio(&text, &format, sample_rate, speed, context_texts, emotion)
            .await
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
        let encoded_audio_len = encoded_audio.len();

        let fetch_completed_ms = started_at.elapsed().as_millis();

        let pcm = decode_audio_to_pcm(&encoded_audio, &format, sample_rate)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
        let pcm_len = pcm.len();
        let pcm_ready_ms = started_at.elapsed().as_millis();

        send_pcm(pcm).await;

        let total_ms = started_at.elapsed().as_millis();
        let playback_duration_ms = pcm_len as u128 * 1000 / (sample_rate as u128 * 2);
        let remaining_ms = playback_duration_ms.saturating_sub(total_ms);

        crate::pylog!(
            "[TTS] Non-stream summary: format={}, fetch={} ms, pcm_ready={} ms, total={} ms, encoded={} bytes, pcm={} bytes, remaining={} ms",
            format,
            fetch_completed_ms,
            pcm_ready_ms,
            total_ms,
            encoded_audio_len,
            pcm_len,
            remaining_ms,
        );

        if remaining_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(remaining_ms as u64)).await;
        }

        Ok(())
    })
}

/// Stream TTS without playback and return timing/statistics as JSON.
#[pyfunction]
#[pyo3(signature = (text, app_id, access_key, resource_id, speaker, speed=1.0, format="mp3".to_string(), sample_rate=24000, emotion=None, context_texts=None))]
pub fn tts_stream_collect(
    py: Python<'_>,
    text: String,
    app_id: String,
    access_key: String,
    resource_id: String,
    speaker: String,
    speed: f32,
    format: String,
    sample_rate: u32,
    emotion: Option<String>,
    context_texts: Option<Vec<String>>,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let started_at = Instant::now();
        let client = DoubaoStreamClient::new(app_id, access_key, resource_id, speaker);
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(16);
        let is_pcm_passthrough = format == "pcm";

        let fetch_handle = tokio::spawn({
            let text = text.clone();
            let format = format.clone();
            async move {
                client
                    .stream_audio(&text, &format, sample_rate, speed, context_texts, emotion, tx)
                    .await
            }
        });

        let mut decoder = StreamingDecoder::new(&format, sample_rate);
        let mut accumulated_size: usize = 0;
        let mut encoded_chunks: usize = 0;
        let mut encoded_bytes: usize = 0;
        let mut pcm_chunks: usize = 0;
        let mut pcm_bytes: usize = 0;
        let mut first_encoded_ms: Option<u128> = None;
        let mut first_pcm_ms: Option<u128> = None;

        while let Some(chunk) = rx.recv().await {
            if first_encoded_ms.is_none() {
                first_encoded_ms = Some(started_at.elapsed().as_millis());
            }

            encoded_chunks += 1;
            encoded_bytes += chunk.len();

            if is_pcm_passthrough {
                if first_pcm_ms.is_none() {
                    first_pcm_ms = Some(started_at.elapsed().as_millis());
                }
                pcm_chunks += 1;
                pcm_bytes += chunk.len();
                continue;
            }

            accumulated_size += chunk.len();
            decoder.feed(&chunk);

            if accumulated_size >= STREAM_BUFFER_THRESHOLD {
                match decoder.decode_all() {
                    Ok(pcm) if !pcm.is_empty() => {
                        if first_pcm_ms.is_none() {
                            first_pcm_ms = Some(started_at.elapsed().as_millis());
                        }
                        pcm_chunks += 1;
                        pcm_bytes += pcm.len();
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "stream decode failed: {}",
                            e
                        )));
                    }
                }
                accumulated_size = 0;
            }
        }

        match decoder.decode_all() {
            Ok(pcm) if !pcm.is_empty() => {
                if is_pcm_passthrough {
                    return Ok(json!({
                        "ok": true,
                        "format": format,
                        "sample_rate": sample_rate,
                        "encoded_chunks": encoded_chunks,
                        "encoded_bytes": encoded_bytes,
                        "pcm_chunks": pcm_chunks,
                        "pcm_bytes": pcm_bytes,
                        "first_encoded_ms": first_encoded_ms,
                        "first_pcm_ms": first_pcm_ms,
                        "total_ms": started_at.elapsed().as_millis(),
                    })
                    .to_string());
                }
                if first_pcm_ms.is_none() {
                    first_pcm_ms = Some(started_at.elapsed().as_millis());
                }
                pcm_chunks += 1;
                pcm_bytes += pcm.len();
            }
            Ok(_) => {}
            Err(e) => {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "final stream decode failed: {}",
                    e
                )));
            }
        }

        if let Ok(Err(e)) = fetch_handle.await {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "stream fetch failed: {}",
                e
            )));
        }

        Ok(json!({
            "ok": true,
            "format": format,
            "sample_rate": sample_rate,
            "encoded_chunks": encoded_chunks,
            "encoded_bytes": encoded_bytes,
            "pcm_chunks": pcm_chunks,
            "pcm_bytes": pcm_bytes,
            "first_encoded_ms": first_encoded_ms,
            "first_pcm_ms": first_pcm_ms,
            "total_ms": started_at.elapsed().as_millis(),
        })
        .to_string())
    })
}

/// Decode encoded audio bytes (MP3/OGG/WAV/FLAC) to PCM (16-bit mono).
/// Returns PCM bytes. This is a sync function for file upload use cases.
#[pyfunction]
#[pyo3(signature = (audio_data, format="mp3", sample_rate=24000))]
pub fn decode_audio<'py>(
    py: Python<'py>,
    audio_data: &[u8],
    format: &str,
    sample_rate: u32,
) -> PyResult<Bound<'py, PyBytes>> {
    let pcm = decode_audio_to_pcm(audio_data, format, sample_rate)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
    Ok(PyBytes::new(py, &pcm))
}

pub fn init_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(tts_stream_play, m)?)?;
    m.add_function(wrap_pyfunction!(tts_play, m)?)?;
    m.add_function(wrap_pyfunction!(tts_stream_collect, m)?)?;
    m.add_function(wrap_pyfunction!(decode_audio, m)?)?;
    Ok(())
}
