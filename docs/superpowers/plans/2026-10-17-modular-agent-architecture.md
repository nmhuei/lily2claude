# Modular Agent Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor all local and remote agent integrations in `lily2claude` from monolithic/hardcoded paths into a fully modular, trait-driven architecture (`src/agents/`) with an extensible `AgentRegistry`.

**Architecture:** Define a standard `Agent` trait with discovery, diagnostics, auth status, model extraction, and turn execution. Implement dedicated modules for `commandcode`, `kilo`, `cline`, `opencode`, and `generic_acp`. Update `AgentRegistry`, `CatalogRegistry`, `Router`, `Server`, and `Doctor` to operate strictly through the modular trait interface without hardcoded paths.

**Tech Stack:** Rust 2021, Tokio, Axum, Serde, Reqwest, Async-trait.

---

### Task 1: Core Agent Trait, Types, and AgentRegistry

**Files:**
- Create: `src/agents/mod.rs`
- Create: `src/agents/registry.rs`
- Modify: `src/lib.rs`
- Test: `tests/test_agents.rs`

**Interfaces:**
- Produces: `pub trait Agent: Send + Sync`, `AuthStatus`, `AgentDiagnostic`, `AgentRegistry`

- [ ] **Step 1: Write test for Agent trait and AgentRegistry**
- [ ] **Step 2: Implement `src/agents/mod.rs` and `src/agents/registry.rs`**
- [ ] **Step 3: Register `pub mod agents;` in `src/lib.rs`**
- [ ] **Step 4: Run `cargo test --test test_agents` and verify pass**

---

### Task 2: Implement Modular Agent Implementations

**Files:**
- Create: `src/agents/commandcode.rs`
- Create: `src/agents/kilo.rs`
- Create: `src/agents/cline.rs`
- Create: `src/agents/opencode.rs`
- Create: `src/agents/generic_acp.rs`
- Test: `tests/test_agents.rs`

**Interfaces:**
- Produces: `CommandCodeAgent`, `KiloAgent`, `ClineAgent`, `OpenCodeAgent`, `GenericAcpAgent` implementing `Agent`

- [ ] **Step 1: Write tests for individual agent model extraction and diagnostics**
- [ ] **Step 2: Implement `commandcode.rs` (cache discovery, auth check, ACP stdio)**
- [ ] **Step 3: Implement `kilo.rs` (models.json parser, auth check, ACP stdio)**
- [ ] **Step 4: Implement `cline.rs` (providers.json parser, auth check, ACP stdio)**
- [ ] **Step 5: Implement `opencode.rs` (WARP socks5 pool, Cloud Zen HTTP)**
- [ ] **Step 6: Implement `generic_acp.rs` (generic ACP agent constructor)**
- [ ] **Step 7: Run `cargo test --test test_agents` and verify all agents pass**

---

### Task 3: Refactor Config, Discovery Catalog, and Router

**Files:**
- Modify: `src/config/mod.rs`
- Modify: `src/discovery/catalog.rs`
- Modify: `src/engine/router.rs`
- Test: `tests/test_config_discovery.rs`
- Test: `tests/test_router.rs`

**Interfaces:**
- Consumes: `AgentRegistry`, `Agent`
- Produces: Dynamic model listing, provider dispatching, candidate alias resolution

- [ ] **Step 1: Update `CatalogRegistry` to discover models dynamically via `AgentRegistry`**
- [ ] **Step 2: Update `Config::load_default()` to initialize default aliases using agent module recommendations**
- [ ] **Step 3: Fix `tests/test_router.rs` priority test (ensure local agent takes precedence)**
- [ ] **Step 4: Run `cargo test --test test_config_discovery` and `cargo test --test test_router` and verify pass**

---

### Task 4: Refactor Server Execution and Doctor Diagnostics

**Files:**
- Modify: `src/server.rs`
- Modify: `src/main.rs`
- Test: `tests/test_cli.rs`

**Interfaces:**
- Consumes: `AgentRegistry`
- Produces: Dynamic `l2c doctor` output iterating all registered agents, dynamic `server` dispatching

- [ ] **Step 1: Refactor `src/server.rs` to dispatch candidates via `AgentRegistry`**
- [ ] **Step 2: Refactor `src/main.rs` doctor command to dynamically display agent diagnostics from `AgentRegistry`**
- [ ] **Step 3: Run all unit and integration tests (`cargo test`) and verify 100% pass**

---

### Task 5: Build Release, Deploy Binaries, and End-to-End Live Verification

**Files:**
- Target: `/home/light/GitHub/lily2claude/target/release/lily2claude`
- Target: `~/.local/bin/lily2claude`
- Target: `~/.local/bin/l2c`

- [ ] **Step 1: Compile optimized release binary with `cargo build --release`**
- [ ] **Step 2: Install updated binaries to `target/release/lily2claude` and `~/.local/bin/`**
- [ ] **Step 3: Restart daemon server (`l2c serve --port 4001`)**
- [ ] **Step 4: Run `l2c doctor` and `l2c list` to verify zero-hardcode modular diagnostics**
- [ ] **Step 5: Run end-to-end live test via `l2c -p "say 'MODULAR_AGENT_VERIFIED'"`**
- [ ] **Step 6: Git commit and conclude goal**
