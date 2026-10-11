local PROVIDER = "runinfra"
local KEY_ENV = "RUNINFRA_GATEWAY_KEY"
local LEGACY_KEY_ENV = "RUNINFRA_API_KEY"
local KEY = "sk-test"
local OTHER_KEY = "sk-other"

local function load(provider, vars)
  local registered
  if provider then
    provider.register = function(declaration)
      registered = declaration
    end
  end
  local environment = setmetatable({
    maki = {
      provider = provider,
      uv = {
        os_getenv = function(name)
          return (vars or {})[name]
        end,
      },
    },
    require = function(name)
      assert(name == "maki.provider_parse")
      return {}
    end,
  }, { __index = _G })
  assert(loadfile("plugin/runinfra.lua", "t", environment))()
  return registered
end

local function selected()
  return {
    is_selected = function(slug)
      assert(slug == PROVIDER)
      return true
    end,
  }
end

assert(load(nil) == nil)
assert(load({}) == nil)
assert(load({
  is_selected = function()
    return false
  end,
}) == nil)

assert(load(selected()).api_key_env == KEY_ENV)
assert(load(selected(), { [KEY_ENV] = KEY }).api_key_env == KEY_ENV)
assert(load(selected(), { [LEGACY_KEY_ENV] = KEY }).api_key_env == LEGACY_KEY_ENV)
assert(load(selected(), { [KEY_ENV] = KEY, [LEGACY_KEY_ENV] = OTHER_KEY }).api_key_env == KEY_ENV)
assert(load(selected(), { [KEY_ENV] = "   ", [LEGACY_KEY_ENV] = KEY }).api_key_env == LEGACY_KEY_ENV)
