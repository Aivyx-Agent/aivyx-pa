# Chatting and running missions

Two screens cover the day-to-day work: **Chat** for a conversation, **Missions**
for larger multi-step jobs.

## Chat

The **Chat** screen is a direct conversation with your assistant — type a
message, get a reply. As it works it may use *tools* (reading a file, searching
the web, writing to memory); you'll see each tool call as it happens.

A few things worth knowing:

- **It streams.** Replies appear as they're generated.
- **You can cancel.** If a turn is taking too long or heading the wrong way,
  cancel it and try again.
- **It remembers.** Within a conversation it keeps context; across conversations
  it draws on its long-term memory (see the Memory page).
- **Risky steps pause.** If a reply requires something irreversible — sending a
  message, deleting a file — the assistant stops and asks you to approve first.

### When it asks first

Some actions can't be undone — deleting or overwriting one of your files,
committing to a git repository, sending an email, placing an order. When the
assistant wants to do one of these, it **pauses and asks you**:

- **In the Studio**, an *Approval needed* card appears in Chat with what it wants
  to do, why it's asking, and the exact details (under *Arguments*). Click
  **Approve** or **Deny**.
- **In the terminal**, it shows the same thing and asks `Approve? [y/N]`. Type
  `y` to approve; Enter (or anything else) says no.

If you approve, it does exactly what it showed you and carries on. If you deny —
or don't answer within 10 minutes — it doesn't, and it tells you so. The
assistant can never approve on your behalf.

In the chat apps (Telegram, Discord, Slack) there's no card: the assistant tells
you what it wants to do and stops, and replying "yes" in your next message lets
it go ahead once.

If you press Ctrl-C while the terminal is asking, the reply is cancelled; press
Enter to get back to the chat.

## Missions (orchestration)

A **mission** is a larger goal you hand off — something that takes several steps,
or several specialists working together. The Missions screen shows each mission's
plan, progress, and any points where it's waiting on you.

How a mission works:

1. You state a goal.
2. The assistant breaks it into a plan of steps.
3. It works through the steps, in order or in parallel where it can.
4. When a step needs your sign-off (an **approval gate**), the mission pauses and
   surfaces the decision. You approve or reject, and it continues.
5. When it's done, you get a summary of what happened.

Missions are where Aivyx PA's **teams** come in — for a complex job, a lead
assistant can delegate steps to a crew of specialists, each with only the access
its job needs. See the [Teams](07-teams.md) page for how that's set up.

### Approval gates

Gates are the heart of the safety model for autonomous work. The assistant can do
all the analysis and preparation unattended, but it **stops at the consequential
step** and waits for a human. You'll see gates both here and in Chat whenever an
action crosses that line.
