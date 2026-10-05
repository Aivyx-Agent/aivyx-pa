# Memory — how your assistant remembers

Your assistant has a long-term memory that grows as you work together. It's
stored encrypted on your machine and is the reason it gets more useful over time
instead of starting fresh every conversation.

Three Studio screens give you a window into it: **Memory**, **Wiki**, and
**Graph**.

## What it remembers

As you talk, the assistant records durable facts worth keeping — your
preferences, ongoing projects, decisions, useful context. It doesn't memorize
every message; it keeps what's likely to matter later. When you ask something, it
pulls back the relevant pieces to inform its answer.

Recall finds memories by keyword and — when an embeddings model is set up —
by meaning, so asking about "the trip to Japan" finds the note that says
"flights to Tokyo booked for May". Setup arranges embeddings for you with Ollama (it offers to
download a small model) and OpenAI; Anthropic has no embeddings service, so
with Anthropic alone recall is keyword-only until you add an `[embedding]`
section.

## The Memory screen

Browse everything your assistant has learned, organized by topic. Search by
keyword or by meaning, and read individual entries. This is the plain,
entry-by-entry view of its knowledge. It also carries the **Learning**
section, which used to live on the Command Center: over its lookback window,
how many recalls were scored as helpful or not, how many entries were
promoted (kept warm), how many personality proposals came from recall, and
the most helpful topics.

## The Wiki screen

The **Wiki** is your assistant's knowledge consolidated into readable per-topic
pages. Where the Memory screen shows raw entries, the Wiki shows a synthesized
summary of what it knows about a subject, with links to related topics. Think of
it as the encyclopedia your assistant writes about your world.

## The Graph screen

The **Graph** is your assistant's knowledge as a map of things and how they
relate: people, projects and places are the points, and each line is a
named relationship — *works on*, *lives in*, *depends on*. Click a point to
focus on it. It's a good way to see the shape of what your assistant
understands.

## Turning on the full memory

The Wiki and the Graph stay empty until you turn on the **smart** memory
profile, because building them takes model calls in the background. Add
this to your config file and restart the daemon:

```toml
[memory]
profile = "smart"
```

`lite` is the middle ground: smarter recall over what's already stored, with
no extra model calls. See the [configuration reference](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/manual/reference/02-configuration.md)
for the finer settings.

## Privacy

All of this lives in an encrypted file on your machine, unlocked by your
passphrase. It is never uploaded anywhere. If you delete a memory, it's gone.
