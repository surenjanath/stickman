//! The AI director (PRD 3.4): a local LLM in Ollama picks the figure's macro
//! behaviour every few seconds from a small JSON observation of what the user is
//! doing. Runs as a tokio task off the main thread; decisions reach the state
//! machine through an `mpsc` channel (claude.md "AI Director Logic").
//!
//! Ollama's structured output (`format` = JSON schema) keeps small models on
//! rails. If Ollama is down, slow or returns junk, a built-in heuristic director
//! takes over, so the figure never freezes waiting on the network.

use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;

use crate::agent::Mode;
use crate::settings::Language;

/// What the director sees (PRD 3.4 / claude.md payload fields).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Observation {
    pub active_app_name: String,
    pub mouse_velocity_x: f32,
    pub mouse_velocity_y: f32,
    /// Mean cursor speed over the last few seconds, points/s.
    pub mouse_speed_avg: f32,
    pub agent_health: f32,
    pub agent_frustration: f32,
    pub current_state: &'static str,
    pub seconds_in_state: f32,
    /// Times the user clicked (hit) the figure in the last minute.
    pub hits_last_minute: u32,
    /// Seconds since the user last moved the mouse.
    pub user_idle_seconds: f32,
    /// Personality (from the settings), described in words in the prompt.
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub aggression: u8,
    #[serde(skip)]
    pub language: Language,
    /// How much it resents the user, 0..100.
    #[serde(skip)]
    pub grudge: u8,
    /// Its rank, e.g. "level 4 Brawler".
    #[serde(skip)]
    pub rank: String,
    /// Its memories, feelings, beliefs and scheme, in words.
    #[serde(skip)]
    pub mind: String,
}

impl Default for Observation {
    fn default() -> Self {
        Observation {
            active_app_name: String::new(),
            mouse_velocity_x: 0.0,
            mouse_velocity_y: 0.0,
            mouse_speed_avg: 0.0,
            agent_health: 100.0,
            agent_frustration: 0.0,
            current_state: Mode::Idle.as_str(),
            seconds_in_state: 0.0,
            hits_last_minute: 0,
            user_idle_seconds: 0.0,
            name: "Stick".into(),
            aggression: 50,
            language: Language::Cheeky,
            grudge: 0,
            rank: String::new(),
            mind: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Llm,
    Fallback,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    pub mode: Mode,
    pub reason: String,
    /// A short line the figure says out loud (may be empty).
    pub say: String,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub url: String,
    pub model: String,
    pub interval: Duration,
    pub timeout: Duration,
}

impl Config {
    /// `STICKMAN_OLLAMA_URL` (default `http://localhost:11434`), `STICKMAN_OLLAMA_MODEL`
    /// (default `llama3.2:1b`, small and fast), `STICKMAN_DIRECTOR_INTERVAL_S` (4).
    pub fn from_env() -> Config {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Config {
            url: get("STICKMAN_OLLAMA_URL").unwrap_or_else(|| "http://localhost:11434".into()),
            model: get("STICKMAN_OLLAMA_MODEL").unwrap_or_else(|| "llama3.2:1b".into()),
            interval: Duration::from_secs_f32(
                get("STICKMAN_DIRECTOR_INTERVAL_S")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(4.0f32)
                    .max(1.0),
            ),
            timeout: Duration::from_secs(25),
        }
    }
}

const PROMPT: &str = "You direct a mischievous little stick figure that lives on the user's \
macOS desktop, like an animation that came alive. Pick its next behaviour.\n\
States:\n\
- IDLE: wander calmly, explore windows.\n\
- OBSERVE: stop and watch the user curiously.\n\
- SABOTAGE: climb onto the user's active window and stomp on it.\n\
- COMBAT: attack the mouse cursor.\n\
Guidelines: if the figure was hit recently or frustration is high, prefer COMBAT. \
If the user is moving the mouse a lot, OBSERVE or COMBAT. If the user has been idle \
or working quietly for a while, SABOTAGE is fun but don't overdo it. Low health means \
calm down (IDLE). Vary the behaviour; avoid repeating one state forever.\n\
Reply with JSON: {\"state\": one of the states, \"reason\": a short in-character reason, \
\"say\": what the figure says out loud in a speech bubble, at most 6 words, cheeky and in character}.\n\
Situation: ";

/// Plain-English facts: small models reason far better over these than over
/// raw numbers.
/// The figure's personality, in words for the model.
pub fn persona(obs: &Observation) -> String {
    let temper = match obs.aggression {
        0..=24 => {
            "gentle and peaceful: it rarely fights and prefers IDLE or OBSERVE, even when poked"
        }
        25..=59 => "playful: it fights back when provoked",
        60..=84 => "hot-headed: it loves COMBAT and SABOTAGE and picks fights when bored",
        _ => "extremely aggressive: it picks fights constantly and strongly prefers COMBAT",
    };
    let talk = match obs.language {
        Language::Polite => "Its 'say' lines are sweet and family-friendly.",
        Language::Cheeky => "Its 'say' lines are cheeky taunts, without swearing.",
        Language::Salty => {
            "Its 'say' lines are foul-mouthed: casual swearing (damn, hell, crap and the like) \
             is fine, but never slurs, nothing hateful, nothing about real people."
        }
    };
    let name = if obs.name.trim().is_empty() {
        "Stick"
    } else {
        obs.name.trim()
    };
    let grudge = match obs.grudge {
        0..=19 => String::new(),
        20..=59 => format!(" It holds a grudge against the user ({}/100).", obs.grudge),
        _ => format!(
            " It holds a serious grudge against the user ({}/100) and wants payback.",
            obs.grudge
        ),
    };
    let rank = if obs.rank.is_empty() {
        String::new()
    } else {
        format!(" It is a {}.", obs.rank)
    };
    let mind = if obs.mind.is_empty() {
        String::new()
    } else {
        format!(
            "\nIts mind (speak as it would, act on its scheme): {}",
            obs.mind
        )
    };
    format!(
        "The figure is called {name}. Its temperament is {temper}.{rank}{grudge} {talk}{mind}\n"
    )
}

pub fn situation(obs: &Observation) -> String {
    let mut facts = Vec::new();
    let app = if obs.active_app_name.is_empty() {
        "nothing in particular"
    } else {
        &obs.active_app_name
    };
    facts.push(format!("The user is using {app}."));
    facts.push(match obs.hits_last_minute {
        0 => "Nobody has hit the figure recently.".to_string(),
        1 => "The user hit the figure once in the last minute.".to_string(),
        n => format!("The user hit the figure {n} times in the last minute!"),
    });
    let level = |v: f32| match v {
        v if v >= 70.0 => "very high",
        v if v >= 40.0 => "medium",
        _ => "low",
    };
    facts.push(format!(
        "Frustration is {} ({:.0}/100).",
        level(obs.agent_frustration),
        obs.agent_frustration
    ));
    facts.push(format!("Health is {:.0}/100.", obs.agent_health));
    facts.push(if obs.user_idle_seconds > 15.0 {
        format!(
            "The mouse has not moved for {:.0} seconds.",
            obs.user_idle_seconds
        )
    } else if obs.mouse_speed_avg > 900.0 {
        "The user is moving the mouse a lot.".to_string()
    } else {
        "The user is moving the mouse calmly.".to_string()
    });
    facts.push(format!(
        "Current state: {} for {:.0} seconds.",
        obs.current_state, obs.seconds_in_state
    ));
    facts.join(" ")
}

/// Body for `POST /api/generate`.
pub fn build_request(cfg: &Config, obs: &Observation) -> serde_json::Value {
    let obs_json = format!(
        "{}\nRaw data: {}",
        situation(obs),
        serde_json::to_string(obs).unwrap_or_default()
    );
    json!({
        "model": cfg.model,
        "prompt": format!("{PROMPT}{}{obs_json}", persona(obs)),
        "stream": false,
        "think": false,
        "keep_alive": "15m",
        "format": {
            "type": "object",
            "properties": {
                "state": {"type": "string", "enum": ["IDLE", "OBSERVE", "SABOTAGE", "COMBAT"]},
                "reason": {"type": "string"},
                "say": {"type": "string"}
            },
            "required": ["state", "reason", "say"]
        },
        "options": {"temperature": 0.8, "num_predict": 100}
    })
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

#[derive(Deserialize)]
struct Decision {
    state: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    say: String,
}

/// Parse an Ollama `/api/generate` body into a directive. Tolerates code fences and
/// chatter around the JSON object.
pub fn parse_response(body: &str) -> Result<Directive> {
    let outer: GenerateResponse =
        serde_json::from_str(body).context("not an Ollama generate response")?;
    let text = outer.response.trim();
    let (start, end) = (text.find('{'), text.rfind('}'));
    let inner = match (start, end) {
        (Some(s), Some(e)) if e > s => &text[s..=e],
        _ => return Err(anyhow!("no JSON object in model output: {text:?}")),
    };
    let d: Decision =
        serde_json::from_str(inner).with_context(|| format!("bad decision JSON: {inner}"))?;
    let mode = Mode::parse(&d.state).ok_or_else(|| anyhow!("unknown state {:?}", d.state))?;
    let reason: String = d.reason.chars().take(120).collect();
    let say: String = d.say.trim().trim_matches('"').chars().take(60).collect();
    Ok(Directive {
        mode,
        reason,
        say,
        source: Source::Llm,
    })
}

/// Built-in director used when the LLM is unavailable.
pub fn fallback(obs: &Observation, roll: f32) -> Directive {
    let (mode, reason, say) = if obs.agent_health < 35.0 {
        (Mode::Idle, "too hurt to fight", "I need a nap.")
    } else if obs.hits_last_minute >= 2 || obs.agent_frustration > 55.0 {
        (
            Mode::Combat,
            "you hit me, now it's war",
            "That's it. It's on!",
        )
    } else if obs.aggression >= 75
        && obs.current_state != "COMBAT"
        && obs.seconds_in_state > 15.0
        && roll < (f32::from(obs.aggression) - 50.0) / 60.0
    {
        (
            Mode::Combat,
            "itching for a fight",
            "Come on, cursor. Fight me!",
        )
    } else if obs.mouse_speed_avg > 900.0 {
        (
            Mode::Observe,
            "what are you doing with that mouse?",
            "Whoa, easy there.",
        )
    } else if obs.user_idle_seconds > 20.0 && roll < 0.5 && obs.aggression > 20 {
        (
            Mode::Sabotage,
            "nobody's looking, time for mischief",
            "Nobody's looking…",
        )
    } else if obs.seconds_in_state > 25.0 {
        match obs.current_state {
            "IDLE" if roll < 0.4 && obs.aggression > 20 => {
                (Mode::Sabotage, "bored, let's stomp a window", "So bored.")
            }
            "IDLE" => (
                Mode::Observe,
                "let's see what you're up to",
                "Whatcha doing?",
            ),
            _ => (Mode::Idle, "taking a break", "Break time."),
        }
    } else {
        return Directive {
            mode: Mode::parse(obs.current_state).unwrap_or(Mode::Idle),
            reason: "carry on".into(),
            say: String::new(),
            source: Source::Fallback,
        };
    };
    Directive {
        mode,
        reason: reason.into(),
        say: say.into(),
        source: Source::Fallback,
    }
}

/// One LLM round trip.
pub async fn ask(client: &reqwest::Client, cfg: &Config, obs: &Observation) -> Result<Directive> {
    let body = client
        .post(format!("{}/api/generate", cfg.url.trim_end_matches('/')))
        .timeout(cfg.timeout)
        .json(&build_request(cfg, obs))
        .send()
        .await
        .context("Ollama unreachable")?
        .error_for_status()
        .context("Ollama returned an error")?
        .text()
        .await?;
    parse_response(&body)
}

/// Run reflections on `rt`: each prompt that arrives on `rx` is sent to the
/// model; the parsed reflection (or `None` if the model failed) goes to `tx`.
pub fn spawn_reflector(
    rt: &tokio::runtime::Runtime,
    cfg: Config,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    tx: Sender<Option<crate::mind::Reflection>>,
) {
    rt.spawn(async move {
        let Ok(client) = reqwest::Client::builder().build() else {
            return;
        };
        while let Some(prompt) = rx.recv().await {
            let body = json!({
                "model": cfg.model,
                "prompt": prompt,
                "stream": false,
                "think": false,
                "keep_alive": "15m",
                "format": {
                    "type": "object",
                    "properties": {
                        "beliefs": {"type": "array", "items": {"type": "string"}},
                        "scheme": {"type": "string"},
                        "note": {"type": "string"},
                        "thought": {"type": "string"}
                    },
                    "required": ["beliefs", "scheme", "note", "thought"]
                },
                "options": {"temperature": 0.9, "num_predict": 200}
            });
            let answer = async {
                let text = client
                    .post(format!("{}/api/generate", cfg.url.trim_end_matches('/')))
                    .timeout(cfg.timeout)
                    .json(&body)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                crate::mind::parse_reflection(&text)
            }
            .await;
            if let Err(e) = &answer {
                log::debug!("mind: reflection failed: {e:#}");
            }
            if tx.send(answer.ok()).is_err() {
                return;
            }
        }
    });
}

/// Run free-text writing jobs (diary entries) on `rt`: each `(id, prompt)`
/// gets `(id, Some(text))` back, or `None` if the model failed.
pub fn spawn_writer(
    rt: &tokio::runtime::Runtime,
    cfg: Config,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<(String, String)>,
    tx: Sender<(String, Option<String>)>,
) {
    rt.spawn(async move {
        let Ok(client) = reqwest::Client::builder().build() else {
            return;
        };
        while let Some((id, prompt)) = rx.recv().await {
            let body = json!({
                "model": cfg.model,
                "prompt": prompt,
                "stream": false,
                "think": false,
                "keep_alive": "15m",
                "format": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                },
                "options": {"temperature": 0.9, "num_predict": 260}
            });
            let answer = async {
                let text = client
                    .post(format!("{}/api/generate", cfg.url.trim_end_matches('/')))
                    .timeout(cfg.timeout)
                    .json(&body)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                #[derive(Deserialize)]
                struct Outer {
                    response: String,
                }
                #[derive(Deserialize)]
                struct Inner {
                    text: String,
                }
                let outer: Outer = serde_json::from_str(&text)?;
                let cleaned: String = outer
                    .response
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                let inner: Inner = serde_json::from_str(cleaned.trim())?;
                anyhow::Ok(inner.text)
            }
            .await;
            if let Err(e) = &answer {
                log::debug!("writer: failed: {e:#}");
            }
            if tx.send((id, answer.ok())).is_err() {
                return;
            }
        }
    });
}

/// Run the director loop on `rt`. Reads the latest observation from `obs`, sends
/// each decision to `tx` while `active` is set; stops when the receiver is gone.
pub fn spawn(
    rt: &tokio::runtime::Runtime,
    cfg: Config,
    obs: watch::Receiver<Observation>,
    tx: Sender<Directive>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    rt.spawn(async move {
        let client = match reqwest::Client::builder().build() {
            Ok(c) => c,
            Err(e) => {
                log::error!("director: HTTP client: {e:#}");
                return;
            }
        };
        log::info!(
            "director: model {} at {} every {:?}",
            cfg.model,
            cfg.url,
            cfg.interval
        );
        let mut llm_ok = true;
        let mut seed: u32 = 0x9e37_79b9;
        loop {
            tokio::time::sleep(cfg.interval).await;
            // Paused while the behaviour is pinned or the figure is hidden: don't
            // spend GPU/CPU on the model.
            if !active.load(std::sync::atomic::Ordering::Relaxed) {
                continue;
            }
            let o = obs.borrow().clone();
            let started = std::time::Instant::now();
            // One retry: small models occasionally emit malformed JSON.
            let answer = match ask(&client, &cfg, &o).await {
                Ok(d) => Ok(d),
                Err(e) => {
                    log::debug!("director: retrying after {e:#}");
                    ask(&client, &cfg, &o).await
                }
            };
            let d = match answer {
                Ok(d) => {
                    if !llm_ok {
                        log::info!("director: Ollama is back");
                    }
                    llm_ok = true;
                    d
                }
                Err(e) => {
                    if llm_ok {
                        log::warn!(
                            "director: {e:#}; using the built-in director until Ollama responds"
                        );
                    }
                    llm_ok = false;
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    fallback(&o, (seed % 1000) as f32 / 1000.0)
                }
            };
            log::info!(
                "director: {} ({:?}, {} ms) \"{}\" says \"{}\"",
                d.mode.as_str(),
                d.source,
                started.elapsed().as_millis(),
                d.reason,
                d.say
            );
            if tx.send(d).is_err() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            url: "http://localhost:11434".into(),
            model: "llama3.2:1b".into(),
            interval: Duration::from_secs(4),
            timeout: Duration::from_secs(25),
        }
    }

    #[test]
    fn request_carries_payload_fields_and_schema() {
        let obs = Observation {
            active_app_name: "Xcode".into(),
            mouse_velocity_x: 12.5,
            agent_health: 80.0,
            ..Default::default()
        };
        let r = build_request(&cfg(), &obs);
        let prompt = r["prompt"].as_str().unwrap();
        for field in [
            "active_app_name",
            "mouse_velocity_x",
            "mouse_velocity_y",
            "agent_health",
            "Xcode",
        ] {
            assert!(prompt.contains(field), "prompt lacks {field}");
        }
        assert_eq!(r["stream"], false);
        assert_eq!(
            r["format"]["properties"]["state"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(r["model"], "llama3.2:1b");
    }

    #[test]
    fn situation_states_the_key_facts_in_words() {
        let obs = Observation {
            hits_last_minute: 4,
            agent_frustration: 90.0,
            user_idle_seconds: 1.0,
            ..Default::default()
        };
        let s = situation(&obs);
        assert!(s.contains("hit the figure 4 times"), "{s}");
        assert!(s.contains("very high"), "{s}");
        assert!(s.contains("calmly"), "{s}");
    }

    #[test]
    fn parses_structured_output() {
        let body = r#"{"model":"x","response":"{\"state\":\"COMBAT\",\"reason\":\"you poked me\",\"say\":\"Put 'em up!\"}","done":true}"#;
        let d = parse_response(body).unwrap();
        assert_eq!(
            (d.mode, d.reason.as_str(), d.say.as_str(), d.source),
            (Mode::Combat, "you poked me", "Put 'em up!", Source::Llm)
        );
        // Older models may leave `say` out.
        let body = r#"{"response":"{\"state\":\"IDLE\",\"reason\":\"meh\"}"}"#;
        assert_eq!(parse_response(body).unwrap().say, "");
    }

    #[test]
    fn parses_chatty_or_fenced_output_and_prd_names() {
        let body = r#"{"response":"Sure!\n```json\n{\"state\": \"sabotage_work\", \"reason\": \"bored\"}\n```"}"#;
        assert_eq!(parse_response(body).unwrap().mode, Mode::Sabotage);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_response("not json").is_err());
        assert!(parse_response(r#"{"response":"I think combat"}"#).is_err());
        assert!(parse_response(r#"{"response":"{\"state\":\"DANCE\"}"}"#).is_err());
    }

    #[test]
    fn personality_reaches_the_prompt_and_the_fallback() {
        let fierce = Observation {
            name: "Brawler".into(),
            aggression: 95,
            language: Language::Salty,
            seconds_in_state: 20.0,
            ..Default::default()
        };
        let p = build_request(&cfg(), &fierce)["prompt"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            p.contains("Brawler") && p.contains("extremely aggressive"),
            "{p}"
        );
        assert!(p.contains("never slurs"), "salty still has limits");
        assert!(
            !p.contains("\"name\""),
            "personality isn't dumped as raw data"
        );
        assert_eq!(fallback(&fierce, 0.1).mode, Mode::Combat);
        let gentle = Observation {
            aggression: 5,
            user_idle_seconds: 60.0,
            ..Default::default()
        };
        assert_ne!(fallback(&gentle, 0.1).mode, Mode::Sabotage);
        let p = build_request(&cfg(), &gentle)["prompt"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(p.contains("gentle"));
    }

    #[test]
    fn fallback_reacts_to_abuse_injury_and_boredom() {
        let base = Observation::default();
        let hit = Observation {
            hits_last_minute: 3,
            ..base.clone()
        };
        assert_eq!(fallback(&hit, 0.5).mode, Mode::Combat);
        let hurt = Observation {
            agent_health: 20.0,
            hits_last_minute: 5,
            ..base.clone()
        };
        assert_eq!(fallback(&hurt, 0.5).mode, Mode::Idle);
        let busy = Observation {
            mouse_speed_avg: 1500.0,
            ..base.clone()
        };
        assert_eq!(fallback(&busy, 0.5).mode, Mode::Observe);
        let idle_user = Observation {
            user_idle_seconds: 60.0,
            ..base.clone()
        };
        assert_eq!(fallback(&idle_user, 0.1).mode, Mode::Sabotage);
        let calm = Observation {
            seconds_in_state: 3.0,
            ..base
        };
        assert_eq!(fallback(&calm, 0.5).mode, Mode::Idle);
    }

    /// Live round trip against the local Ollama, when it's running.
    #[test]
    fn live_ollama_round_trip() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let c = cfg();
        let client = reqwest::Client::new();
        let up = rt.block_on(async {
            client
                .get(format!("{}/api/version", c.url))
                .timeout(Duration::from_secs(1))
                .send()
                .await
                .is_ok()
        });
        if !up {
            eprintln!("Ollama not running; skipping live director test");
            return;
        }
        let obs = Observation {
            hits_last_minute: 4,
            agent_frustration: 90.0,
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let d = rt
            .block_on(ask(&client, &c, &obs))
            .expect("Ollama answered with a valid directive");
        eprintln!(
            "live director: {:?} \"{}\" in {:?}",
            d.mode,
            d.reason,
            started.elapsed()
        );
        assert_eq!(d.source, Source::Llm);
    }
}
