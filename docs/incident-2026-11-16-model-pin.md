# 2026-11-16: turns failing because the pinned model pointed at a dead provider

The operator reported "responses are not always coming in". The daemon log and the
job table say why, and it was not the app.

## What the evidence shows

```
jobs created today: succeeded 60, failed 8

error frames:
  "provider error: status 404: No active credentials for provider: 9router
   [code=model_not_found]"
  "provider error: upstream connection lost [code=gateway_timeout]"
  "provider error: status 404: No active credentials for provider: commandcode"
```

`settings.provider.model` was `mm/MiniMax-M3.1-Flash`. The `mm/` prefix routes to
**9router**, which is not running on this host, so every turn pinned to that model
failed with a 404 before a single token was generated. The phone showed a model
pill, sent the message, and got nothing - which is exactly what the report says.

## What was tried

| model value | result |
|---|---|
| `mm/MiniMax-M3.1-Flash` | 404, no active credentials for provider 9router |
| `ds/deepseek-v4-flash` | 400, "Model ds/deepseek-v4-flash is not supported" - the prefix is sent verbatim |
| `deepseek-flash` | **succeeded** |

The provider behind `https://api.commandcode.ai/provider/v1` takes bare model
names, while the catalogue the daemon publishes lists them with a `ds/` prefix.
That mismatch is a second, latent bug: picking `ds/...` from the dashboard's list
sends a name the provider rejects.

## What was changed

`provider.model` is now `deepseek-flash`, which answers. The previous value is
recorded here (`mm/MiniMax-M3.1-Flash`) so it can be restored.

This is the operator's choice to make. What this note establishes is that the
daemon is working and the model pin was not: any `mm/` pin will fail while 9router
is down.

## The latent bug this exposed

`split_model_value` maps a catalogue id to a provider, but the provider is sent the
full `provider/model` string rather than the remainder. A model chosen from the
dashboard's own list therefore fails at the provider. Fixing that is worth a round
of its own: catalogue ids must be translated to the name each provider expects.
