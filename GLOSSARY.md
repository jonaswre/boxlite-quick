# boxq

A tool for running AI coding agents against a host project inside an isolated micro-VM, as an open alternative to Docker's sbx.

## Language

**Sandbox**:
The isolated environment boxq creates to run one Agent against one Workspace. The only isolation unit users see.
_Avoid_: Box, VM, container, environment

**Box**:
BoxLite's micro-VM primitive, which a Sandbox runs in. An implementation detail, never user-facing.
_Avoid_: Sandbox, VM

**Workspace**:
The host directory shared into a Sandbox for the Agent to work on.
_Avoid_: Project, mount, volume

**Base Image**:
boxq's own image, which a Sandbox is created from unless the config sets another. A generic core with the Agent preinstalled, and none of boxq's config.
_Avoid_: Default image, container image, base

**Agent**:
The AI coding tool run inside a Sandbox, such as Claude Code.
_Avoid_: Assistant, bot, model

**Egress Policy**:
The rules deciding which outbound destinations a Sandbox may reach.
_Avoid_: Firewall, allowlist, network policy

**Global Config**:
The user's own boxq settings, applying to every Sandbox. The only place the Agent's credentials and host-loopback access can be set.
_Avoid_: User config, settings

**Workspace Config**:
The boxq settings a Workspace carries with it, usually committed to its repo. Untrusted, since whoever wrote the repo wrote it.
_Avoid_: Project config, repo config, local config

**Egress Approval**:
The user's recorded consent to a Workspace Config's Egress Policy additions. Lapses whenever those additions change.
_Avoid_: Trust, allow
