-- Reading one inbox connection: newline-delimited lines, the frame size
-- limit, and the idle deadline, over an injected connection and timer so
-- tests/spec.lua can drive both by hand.
--
-- io = {
--   read = function() -> chunk?, err?  -- nil, nil at a clean end of stream
--   close = function()
--   after = function(ms, fn) -> cancel  -- runs fn after ms unless cancelled
--   on_line = function(line)
--   max_bytes, idle_ms
-- }

local M = {}

-- Lines until the sender closes, a line reaches io.max_bytes, the connection
-- breaks, or io.idle_ms pass without a complete line. A remainder after the
-- last newline counts as a line only at a clean end of stream: a deadline or
-- a broken read means the sender never finished it. Answers pcall's results.
function M.serve(io)
  local cancel
  local function rearm()
    if cancel then
      cancel()
    end
    cancel = io.after(io.idle_ms, io.close)
  end
  rearm()
  local ok, err = pcall(function()
    local buffer = ""
    while true do
      local chunk, read_err = io.read()
      if not chunk then
        if not read_err and buffer ~= "" then
          io.on_line(buffer)
        end
        return
      end
      buffer = buffer .. chunk
      while true do
        local newline = buffer:find("\n", 1, true)
        if not newline then
          break
        end
        local line = buffer:sub(1, newline - 1)
        buffer = buffer:sub(newline + 1)
        if #line >= io.max_bytes then
          return
        end
        rearm()
        io.on_line(line)
      end
      if #buffer >= io.max_bytes then
        return
      end
    end
  end)
  cancel()
  io.close()
  return ok, err
end

return M
