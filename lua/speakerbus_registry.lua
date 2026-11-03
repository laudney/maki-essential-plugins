-- The registry slot <config>/sessions/<pid>.json, sequenced over injected
-- I/O. Every write, the read-back, and the removal run under one lock, and a
-- shutdown flag set before the lock is taken stops any write that has not
-- started, so cleanup cannot race the first publication.
--
-- io = { read, write, remove, encode, decode, lock }: read/write/remove and
-- encode/decode answer a value or nil plus an error, and lock() answers an
-- unlock function that must be called exactly once.

local M = {}

local Registry = {}
Registry.__index = Registry

local SLOT_TAKEN = "registry slot owned by another process"
local SLOT_LOST = "registry slot taken by another process"
local STOPPED = "stopped"

M.SLOT_TAKEN = SLOT_TAKEN
M.SLOT_LOST = SLOT_LOST
M.STOPPED = STOPPED

function M.new(io, proc_start)
  return setmetatable({ io = io, proc_start = proc_start, stopped = false }, Registry)
end

-- Who the slot's content says owns it: a procStart string, or nil for a
-- missing, unreadable, or foreign-shaped slot.
local function holder(io, text)
  if not text then
    return nil
  end
  local value = io.decode(text)
  if type(value) ~= "table" or type(value.procStart) ~= "string" then
    return nil
  end
  return value.procStart
end

local function changed(old, new)
  return not old or old.sessionId ~= new.sessionId or old.cwd ~= new.cwd or old.status ~= new.status
end

-- Runs fn under the lock and unlocks exactly once, even when fn raises.
function Registry:locked(fn)
  local unlock = self.io.lock()
  local ok, first, second = pcall(fn)
  unlock()
  if not ok then
    error(first, 0)
  end
  return first, second
end

function Registry:write(entry)
  local text, err = self.io.encode(entry)
  if not text then
    return nil, err
  end
  local ok, write_err = self.io.write(self.slot, text)
  if not ok then
    return nil, write_err
  end
  self.written = entry
  return true
end

function Registry:claim(slot)
  self.slot = slot
  local owner = holder(self.io, self.io.read(slot))
  if owner and owner ~= self.proc_start then
    return nil, SLOT_TAKEN
  end
  return true
end

function Registry:publish(entry)
  return self:locked(function()
    if self.stopped then
      return nil, STOPPED
    end
    local ok, err = self:write(entry)
    if not ok then
      return nil, err
    end
    local text, read_err = self.io.read(self.slot)
    if not text then
      return nil, read_err
    end
    if holder(self.io, text) ~= self.proc_start then
      return nil, SLOT_LOST
    end
    self.published = true
    return true
  end)
end

function Registry:update(entry)
  return self:locked(function()
    if self.stopped or not self.published or not changed(self.written, entry) then
      return true
    end
    return self:write(entry)
  end)
end

function Registry:shutdown()
  self.stopped = true
  return self:locked(function()
    if not self.slot or holder(self.io, self.io.read(self.slot)) ~= self.proc_start then
      return true
    end
    return self.io.remove(self.slot)
  end)
end

return M
