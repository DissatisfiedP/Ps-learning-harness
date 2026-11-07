# Claude subscription provider (Claude Pro/Max)

`crates/providers/src/claude_oauth.rs` connects a Claude Pro/Max/Team subscription through the same OAuth sign-in the Claude Code CLI uses — no API key. The learner approves in a browser and pastes the one-time code back into Settings. Investigated 2026-11-06.

This is an **undocumented, consumer-plan** integration. Anthropic can change or block it at any time. Requests count against the subscription's usage limits, not an API-key balance. Use your own account, at your own risk. It is not an official Anthropic API.

## Where the facts come from

| Mark | Source |
|---|---|
| **CLI** | Constants read from the Claude Code CLI source (`claude-code/src/constants/oauth.ts`, `services/oauth/client.ts`, `services/api/client.ts`, `constants/betas.ts`) and a working local reference implementation (`claude-method`). |
| **omp** | `omp://provider-quirks.md`, `omp://providers.md`, `omp://auth-broker-gateway.md` confirm the shape: PKCE authorization-code flow, Bearer auth, Claude Code identity, `oauth-2025-04-20` beta, `/api/oauth/usage` quota. These are architecture references and do not inline every string. |
| **[inference]** | A choice we made because the doc does not force it (paste-code instead of loopback; the specific beta set). |

## Sign-in (authorization code + PKCE, paste the code)

1. Generate a PKCE `code_verifier` (64 random alphanumeric chars) and `state` (32 chars). Compute `code_challenge = base64url(sha256(verifier))` (S256, no padding).
2. Build the authorize URL and open it in the system browser:

   | Param | Value |
   |---|---|
   | `code` | `true` |
   | `client_id` | `9d1c250a-e61b-44d9-88ed-5944d1962f5e` (public Claude Code client id) |
   | `response_type` | `code` |
   | `redirect_uri` | `https://platform.claude.com/oauth/code/callback` |
   | `scope` | `user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload` |
   | `code_challenge`, `code_challenge_method` | computed above, `S256` |
   | `state` | random |

   Base: `https://claude.com/cai/oauth/authorize` (the Claude.ai subscriber path; it bounces through claude.com for attribution).
3. After the learner approves, the hosted callback page shows a code in the form `<code>#<state>`. The learner copies it and pastes it into Settings → Providers → Claude (subscription).
4. Exchange at `POST https://platform.claude.com/v1/oauth/token` with a **JSON** body (not form-encoded): `grant_type=authorization_code`, `code`, `redirect_uri` (same manual URL), `client_id`, `code_verifier`, `state`. Response: `access_token`, `refresh_token`, `expires_in` (seconds), and optionally `account.email_address` / `organization.name`. The app stores `expires_at = now + expires_in`, refreshes 5 minutes before expiry.
5. Refresh: `POST` the same token URL with `grant_type=refresh_token`, `refresh_token`, `client_id`, `scope`. The refresh token may rotate; a response that omits it keeps the last good one. A failed refresh clears the grant and asks the learner to sign in again.

**Why paste-code, not a loopback server.** omp uses a loopback redirect on port 54545; the Claude Code CLI also supports an automatic localhost callback. **[inference]** For this desktop app the hosted `MANUAL_REDIRECT_URL` + paste flow is simpler: no local TCP listener, no port conflicts, no CSP/callback-window handling, and it works when the browser is on a different machine. The verifier and `state` never leave the app; only the code comes back.

`ANTHROPIC_OAUTH_TOKEN` in the environment is also accepted as a subscription bearer (no refresh), matching omp's env-variable precedence where an OAuth token beats `ANTHROPIC_API_KEY`. It is treated as a `ClaudeCode` credential, not an API key.

## Inference (Messages API, Bearer)

Same endpoint as an API key: `POST https://api.anthropic.com/v1/messages`, SSE, `wire: AnthropicMessages`. The body is the standard Messages request. Two differences:

- **Headers** (subscription, never `x-api-key`):

  | Header | Value |
  |---|---|
  | `Authorization` | `Bearer <access_token>` |
  | `anthropic-version` | `2023-06-01` |
  | `anthropic-beta` | `claude-code-20250219,oauth-2025-04-20` |
  | `x-app` | `cli` |
  | `User-Agent` | `claude-cli/2.1.280 (external, cli)` |
  | `X-Claude-Code-Session-Id` | the harness session/conversation id |

- **System prompt:** subscription requests must prepend `system[0] = "You are Claude Code, Anthropic's official CLI for Claude."`. The harness's composed system prompt is `system[1]`. This identity is required for the endpoint to accept a Bearer subscription token. `anthropic::body` adds it only when `Endpoint.claude_code` is true (set by `claude_oauth::endpoint`), so API-key requests are unchanged.

The `oauth-2025-04-20` beta enables Bearer OAuth; `claude-code-20250219` identifies the client. omp's full CLI beta set is larger (interleaved-thinking, context-management, etc.); **[inference]** we send only the two that are needed for plain streaming + tool calls, because the harness does not request those beta-only features. A rejected request with "unexpected beta" would mean tightening or widening this set.

## Models

There is no live `/v1/models` discovery for the subscription. The picker suggests `claude-sonnet-4-6` and `claude-opus-4-6`; any id the account serves can be typed in, as with the API-key providers. Suggestions are a starting point, not an allow-list — subscription entitlements are account- and region-specific.

## Quota / errors

- Usage is tracked at `https://api.anthropic.com/api/oauth/usage` (rolling `five_hour`, `seven_day` windows) — omp exposes this; this app does not currently display it.
- A `401` on a request refreshes the grant once and retries. A second `401`, or a failed refresh, surfaces "subscription sign-in expired; sign in again".
- Other Anthropic error envelopes (`error.message`) and gateway HTML are handled by the shared provider error mapping.

## Not done on purpose

- No account rotation, no quota display, no prompt-cache tuning, no 1M-context beta (omp omits it for subscriptions), no tool-name `_` prefixing. The harness uses its own plain tool names; if Anthropic rejects one, that is a tool error rather than a hidden rewrite.
- No rewriting of the learner's system prompt beyond the mandatory identity prefix.
