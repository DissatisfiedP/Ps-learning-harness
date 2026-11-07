# WorkBuddy provider

WorkBuddy (www.workbuddy.ai) is Tencent's desktop agent. It has no public API docs. The provider in `crates/providers/src/workbuddy.rs` follows what Tencent's own client does. Investigated 2026-11-05.

## Where the facts come from

| Mark | Source |
|---|---|
| **official** | Read in Tencent's published CLI bundle `@tencent-ai/codebuddy-code@2.180.0` (npm). `dist/codebuddy-lite-wb.mjs` is the WorkBuddy build of that CLI, which the desktop app runs. |
| **live** | Called against `https://www.workbuddy.ai` from this machine, without an account. |
| **third-party** | Reported by community integrations (`@magpie-community/opencode-workbuddy-auth`, `@huai_xin/dsh-workbuddy`). Not checked with a real account. |

The web app at `www.workbuddy.ai/app` is only a site shell: its bundle has no chat code. The chat client is the CLI.

## Accounts and hosts

- Host: `https://www.workbuddy.ai`. Login platform id: `workbuddy-ai` (**live**: `auth/state?platform=workbuddy-ai` returns a login URL for that platform).
- Same gateway as CodeBuddy: APISIX/openresty, OpenAI-style chat, `{code, msg, data}` envelopes (**live**).
- The China build (`www.workbuddy.cn`, platform `workbuddy`) uses the same protocol. It is not wired in.

## Sign-in (IDE-plugin flow)

1. `POST /v2/plugin/auth/state?platform=workbuddy-ai` with body `{}` and headers `X-Domain: www.workbuddy.ai`, `X-No-Authorization: true`, `X-No-User-Id: true`, `X-No-Enterprise-Id: true`, `X-No-Department-Info: true`.
   - Returns `data.state` and `data.authUrl`. (**official** + **live**)
2. Open `authUrl` in the browser. The learner approves there.
3. Poll `GET /v2/plugin/auth/token?state=<state>` with the same anonymous headers.
   - `code 11217` ("login ing...") means still pending. (**official** constant `LOGIN_TOKEN_PENDING_CODE`; **live**)
   - `code 0` returns `accessToken`, `refreshToken`, `expiresIn`, `refreshExpiresIn` (seconds), and `domain`.
   - The official client polls for up to 5 minutes; this app polls every 1.5 s. (**official**)
4. Refresh with `POST /v2/plugin/auth/token/refresh`, sending `Authorization: Bearer <access>`, `X-Refresh-Token`, `X-Auth-Refresh-Source: plugin`, and `X-Domain`. (**official**)
   - This app refreshes when less than 24 h remain, and once more after a 401.

The access token is a Keycloak JWT (realm `copilot`). The account label shown in Settings comes from its `nickname` / `preferred_username` / `email` claim. The token is decoded only for display, never verified. (**third-party**)

## API keys

Keys are created at <https://www.workbuddy.ai/profile/keys>.

- The official CLI sends a key (`CODEBUDDY_API_KEY`) as both `Authorization: Bearer <key>` and `X-API-Key: <key>`, with no `X-Domain`. (**official**: the API-key session uses `tokenType: "ApiKey"`; the header pairing is **third-party**)
- With an invalid key, chat returns `401 invalid_format` (**live**).

## Chat

`POST /v2/chat/completions` uses the OpenAI chat-completions body with SSE responses.

- `stream` must be `true`. Non-streaming requests get `11101`. (**third-party**)
- The first message must be `system`, otherwise the gateway returns `11128`. (**third-party**)
- `stream_options.include_usage: true` is sent, as the official client does. (**official**)
- `reasoning_effort` is sent only if the model's catalog entry lists it in `reasoning.supportedEfforts`. (**official** field names; **third-party** behaviour)
- `: heartbeat` comment frames are skipped. The stream ends with `data: [DONE]`. A stream that stops without `[DONE]` or a `finish_reason` is reported as cut, not as a normal finish.

Headers on every call, as the WorkBuddy desktop client sets them (**official** header names and defaults; versions are **third-party**, read from desktop build 5.9.3):

| Header | Value |
|---|---|
| `X-Product` | `SaaS` |
| `X-IDE-Type`, `X-IDE-Name` | `WorkBuddy` |
| `X-IDE-Version` | `5.9.3` |
| `User-Agent` | `WorkBuddy/5.9.3 workbuddy-ai/5.9.3 CLI/2.165.0` |
| `X-Agent-Intent` | `craft` (official default) |
| `X-Agent-Purpose` | `conversation` |
| `X-Agent-Runtime` | `local-agent` |
| `X-Conversation-ID` | the P's Learning Harness session id |
| `X-Conversation-Request-ID`, `X-Conversation-Message-ID`, `X-Request-ID` | a new UUID per request |

Requests missing the client headers are reported to fail with `11128` (**third-party**). This is why the app identifies itself as the WorkBuddy client. If Tencent changes the expected versions, update `APP_VERSION` and `CLI_VERSION` in `workbuddy.rs`.

## Models

`GET /v3/config` with the auth headers returns `data.models[]` for the account. Unauthenticated, it returns `models: null` (**live**).

The picker drops non-chat entries, using the same filter as the community integrations:
- `nes-`, `completion-` and `codewise-` prefixes;
- `supportsExtra`;
- `maxOutputTokens ≤ 256`;
- the `text-to-image` tag.

It keeps `maxInputTokens`, `reasoning.supportedEfforts` / `defaultEffort`, `credits` and `descriptionEn`. The `isDefault` model is listed first.

## Errors

Gateway errors are JSON envelopes such as `{"code":11158,"msg":"…","displayMsg":{"en":"…"}}`. The app shows `displayMsg.en`, followed by the code and the raw message. Codes worth knowing (**third-party**):
- `11102`: model not served;
- `11158`: not allowed for this account tier;
- `11128`: rejected as an unapproved client;
- HTTP 429 with code `6020`: queued.

## Risks

- This is an undocumented, consumer-plan API. Tencent can change it or block non-official clients at any time. Requests count against the account's WorkBuddy credits.
- Sending WorkBuddy's own client headers is how the gateway accepts the request. It may conflict with WorkBuddy's terms of service. Use your own account, at your own risk.
- Not done on purpose: the community plugins rewrite system prompts to dodge content fingerprinting and rotate accounts after policy blocks. This app does neither.
