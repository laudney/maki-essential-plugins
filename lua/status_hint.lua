local M = {}
local features = { "goal", "monitor" }
local hints = {}
local focused_session

local function publish()
  local spans = {}
  for _, feature in ipairs(features) do
    local hint = hints[feature]
    if hint and hint.session == focused_session then
      for _, span in ipairs(hint.spans) do
        spans[#spans + 1] = span
      end
    end
  end
  maki.ui.set_status_hint(#spans > 0 and spans or nil)
end

function M.set(feature, session, spans)
  if focused_session and focused_session ~= session then
    return
  end
  focused_session = session
  hints[feature] = spans and { session = session, spans = spans } or nil
  publish()
end

function M.clear(feature, session)
  if hints[feature] and hints[feature].session == session then
    hints[feature] = nil
    publish()
  end
end

maki.api.create_autocmd("SessionFocusChanged", {
  callback = function(event)
    focused_session = event.data.session_id
    publish()
  end,
})

return M
