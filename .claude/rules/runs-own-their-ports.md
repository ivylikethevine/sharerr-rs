---
paths:
  - "**/scripts/**"
  - "**/tests/**"
  - "**/*compose*.y*ml"
---

# A run owns its ports

A script that starts a server, a database or a compose stack must not run its
checks against something else that answers on the same port.

- Probe each port before starting, and refuse one that anything already holds.
- Exit non-zero when the server or database fails to start
  (`address already in use`, `could not start server`, `already exists`), and
  pass that failure up through every wrapper; a chain stops at a failed run.
- Check that what answers is the process the script started (its PID, its
  container ID, or a marker it wrote), not just that the port responds.
- Stop everything the script started when it exits:
  `trap cleanup EXIT INT TERM HUP`, stop the process groups or the compose
  project, then check that the ports are free and warn when one is not.
- Two runs that use the same default ports never run side by side; give one
  its own ports through the environment.

A new script that starts a server gets these guards before its first run. A
result from a run that skipped them is repeated, never reported.
