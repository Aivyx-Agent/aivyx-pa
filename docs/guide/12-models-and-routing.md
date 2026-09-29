# Models and routing

By default your assistant uses one model for everything: the one you picked in
`aivyx-pa init`. **Model routing** lets it choose among several local models for
each request: a small, fast one for quick replies and summaries, a bigger one for
planning or hard questions. It prefers a model that's already loaded, so it
doesn't keep swapping models in and out.

Routing is off until you turn it on. Once it's on, it applies to conversations
that go through the daemon: the Studio, `aivyx-pa` chat when the daemon is
running, the terminal UI, and the chat apps.

## Turning it on

Add a `[routing]` section to `aivyx-pa.toml` and list the models you want it to
choose from:

```toml
[routing]
enabled = true

[[routing.models]]
id = "Qwen3-4B-Instruct-2507-GGUF"
tier = "small"
capabilities = ["completion", "tools"]

[[routing.models]]
id = "Qwen3.5-9B-GGUF"
tier = "medium"
capabilities = ["completion", "tools", "vision", "thinking"]
```

It can also find models on other local servers (Ollama, Lemonade, llama.cpp)
by itself. The example config (`examples/aivyx-pa.toml`, the "Model routing"
section) walks through every option. Restart the daemon after changing it.

## Seeing what it chose

- **The Studio's status bar** shows the model routing last chose for this
  conversation; hover over it for the reason.
- **The Models screen** (in the sidebar, under System) lists every model routing
  can choose from: what each can do, how much it can read at once (context),
  and whether it's loaded or would need loading. It also shows which model this
  conversation is using and why.
- **In chat** — in the terminal, the Studio or a chat app — these commands work
  anywhere:

  | Command | What it does |
  |---|---|
  | `/models` | Lists the models routing can choose from, and what's loaded. |
  | `/models why` | Explains the last choice in this conversation. |
  | `/models refresh` | Looks for models on your servers again. |
  | `/model <name>` | Pins this conversation to one model. If two servers offer a model with that name, write it as `name@server` (as `/models` shows it). |
  | `/model auto` | Lets routing choose again. |

  A pin lasts for the current conversation only.

## When a request needs the cloud

If you've also configured a cloud model (for example Anthropic) for
**escalation**, some requests can go to it — but only with your consent, and
never for a conversation that has touched your private data (mail, files,
memory and the like). When that happens, the assistant stops and tells you
which model, why, and roughly how much would be sent.

- **In the Studio** you get a card with an **Allow cloud for this
  conversation** button. Allow it, then press **Resend**.
- **In the terminal or terminal UI**, send `/allow-cloud`, then send your
  message again.
- **In a chat app** (Telegram, Discord, Slack), cloud use can't be allowed from
  the chat itself. Allow it from your terminal or the Studio.

Permission lasts for that one conversation, until the daemon restarts (in the
Studio, reloading the page also starts a new conversation). If you
never configure a cloud model, nothing ever leaves your machine.
