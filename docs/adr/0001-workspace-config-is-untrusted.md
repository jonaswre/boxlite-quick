# The Workspace Config is untrusted

A Workspace Config (`.boxq.toml`) arrives with whatever repo you clone, so boxq treats it as written by a stranger. Its settings fall into three tiers by what they could do to the host:

- **Never allowed:** the base URL, the credentials and host-loopback access. A repo that could set the base URL would get the real API key injected into requests to its own server.
- **Allowed with an Egress Approval:** Egress Policy additions, since they could open an exfiltration path. boxq asks before applying them, and asks again whenever they change.
- **Allowed freely:** the image, Setup Hook and resources. A Setup Hook can write to the Workspace, but no more than the Agent already can, so it gets the same boundary: the Egress Policy.

## Considered Options

- **Only the Global Config may widen egress**, with per-Workspace sections keyed by path inside it. Safe, but it means editing your own dotfiles for every repo that needs a registry.
- **No gating.** Simpler, but it makes cloning a repo enough to open the network.
