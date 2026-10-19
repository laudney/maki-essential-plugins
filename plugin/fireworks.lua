local PROVIDER = "fireworks-ai"

if not maki.provider or not maki.provider.is_selected or not maki.provider.is_selected(PROVIDER) then
  return
end

local parse = require("maki.provider_parse")

local MODELS_PATH = "/accounts/fireworks/models?filter=supports_serverless%3Dtrue&pageSize=200"

local function encode(value)
  return (value:gsub("[^%w%-._~]", function(char)
    return string.format("%%%02X", string.byte(char))
  end))
end

local function parse_model(model)
  if
    type(model) ~= "table"
    or type(model.name) ~= "string"
    or not model.name:match("^accounts/fireworks/models/[^/%s]+$")
    or model.supportsServerless ~= true
    or type(model.conversationConfig) ~= "table"
  then
    return nil
  end
  local context = parse.as_u32(model.contextLength)
  return {
    id = model.name,
    context_window = context and context > 0 and context or nil,
    -- An omitted protobuf boolean means false.
    supports_vision = model.supportsImageInput == true,
  }
end

maki.provider.register({
  slug = PROVIDER,
  display_name = "Fireworks AI",
  codec = "openai",
  base_url = "https://api.fireworks.ai/inference/v1",
  api_key_env = "FIREWORKS_API_KEY",
  login_url = "https://app.fireworks.ai/settings/users/api-keys",
  default_model = "accounts/fireworks/models/glm-5p3-flash",
  family = "generic",
  accepts_arbitrary_models = true,
  max_output_tokens = false,
  aperture = { path_prefix = "/inference/v1" },
  docs = {
    features = "Serverless chat models and image input",
    discovery_note = "Model IDs, context windows and image support come from the authenticated "
      .. "[Fireworks model listing](https://docs.fireworks.ai/api-reference/list-models). "
      .. "Discovery leaves output limits, reasoning capabilities and token prices unspecified.",
  },
  openai = { thinking = { dialect = "standard", requires_support = true } },

  list_models = function(ctx)
    -- Management uses /v1, while chat uses /inference/v1. Keep gateway prefixes
    -- and the selected origin so discovery follows the user's base URL.
    local base = ctx.base_url:gsub("/+$", ""):gsub("/inference/v1$", "/v1")
    local url = base .. MODELS_PATH
    local models, seen = {}, {}
    while true do
      local body, err = ctx.get_json(url)
      if err then
        return nil, err
      end
      if type(body) ~= "table" or (body.models ~= nil and type(body.models) ~= "table") then
        return nil, maki.provider.http_error({ status = 502, body = "Invalid Fireworks model listing" })
      end
      for _, model in ipairs(body.models or {}) do
        table.insert(models, model)
      end
      local token = body.nextPageToken
      if token == nil or token == "" then
        break
      end
      if type(token) ~= "string" or seen[token] then
        return nil, maki.provider.http_error({ status = 502, body = "Invalid Fireworks model page token" })
      end
      seen[token] = true
      url = base .. MODELS_PATH .. "&pageToken=" .. encode(token)
    end
    return parse.models({ data = models }, parse_model)
  end,
})
