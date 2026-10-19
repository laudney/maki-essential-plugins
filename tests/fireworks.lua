local PROVIDER = "fireworks-ai"

local function load(provider)
  local registered
  if provider then
    provider.register = function(declaration)
      registered = declaration
    end
  end
  local environment = setmetatable({
    maki = { provider = provider },
    require = function(name)
      assert(name == "maki.provider_parse")
      return {}
    end,
  }, { __index = _G })
  assert(loadfile("plugin/fireworks.lua", "t", environment))()
  return registered
end

assert(load(nil) == nil)
assert(load({}) == nil)
assert(load({
  is_selected = function(slug)
    assert(slug == PROVIDER)
    return false
  end,
}) == nil)

local registered = load({
  is_selected = function(slug)
    assert(slug == PROVIDER)
    return true
  end,
})
assert(registered.slug == PROVIDER)
assert(registered.api_key_env == "FIREWORKS_API_KEY")
