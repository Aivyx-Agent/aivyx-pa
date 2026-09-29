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
