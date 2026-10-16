local PICKER_WIDTH = "70%"
local PICKER_HEIGHT = "60%"
local FILTER_ROWS = 1
local EMPTY_MODELS = "No models are listed yet. Log in with maki auth login <provider>, then open /model to refresh."

local picker

local function close_picker()
  if picker then
    local closing = picker
    picker = nil
    closing.win:close()
  end
end

local function choose(items, title, current, session)
  local buf = maki.ui.buf()
  local win = maki.ui.open_win(buf, {
    title = title,
    width = PICKER_WIDTH,
    height = PICKER_HEIGHT,
    cursor_line = true,
    reserved_top = FILTER_ROWS,
    footer = { { "Type", "filter" }, { "↑/↓", "select" }, { "Enter", "confirm" }, { "Esc", "cancel" } },
  })
  local active = { win = win, session = session }
  picker = active
  local query, rows, selected = "", items, 1
  local height = win.height
  for index, item in ipairs(items) do
    if item == current then
      selected = index
      break
    end
  end

  local function render()
    local lines = { { { "Filter: " .. query, "dim" } } }
    for _, item in ipairs(rows) do
      lines[#lines + 1] = "  " .. item .. (item == current and " (current)" or "")
    end
    if #rows == 0 then
      lines[#lines + 1] = "  No matches"
    end
    buf:set_lines(lines)
    win:set_cursor(selected + FILTER_ROWS)
  end

  local function filter(text)
    query, rows, selected = text, {}, 1
    for _, item in ipairs(items) do
      if item:lower():find(query:lower(), 1, true) then
        rows[#rows + 1] = item
      end
    end
  end

  render()
  while picker == active do
    local ev = win:recv()
    if picker ~= active then
      return nil
    end
    if not ev or ev.type == "close" then
      close_picker()
      return nil
    elseif ev.type == "resize" then
      height = ev.height
    elseif ev.type == "paste" then
      filter(query .. ev.text:gsub("%c", ""))
    elseif ev.type == "key" then
      local key = ev.key
      local page = math.max(1, height - FILTER_ROWS)
      if key == "<Esc>" then
        close_picker()
        return nil
      elseif key == "<CR>" and rows[selected] then
        local result = rows[selected]
        close_picker()
        return result
      elseif key == "<Up>" or key == "<C-p>" then
        selected = math.max(1, selected - 1)
      elseif key == "<Down>" or key == "<C-n>" then
        selected = math.min(math.max(1, #rows), selected + 1)
      elseif key == "<PageUp>" then
        selected = math.max(1, selected - page)
      elseif key == "<PageDown>" then
        selected = math.min(math.max(1, #rows), selected + page)
      elseif key == "<BS>" then
        local last = utf8.offset(query, -1)
        filter(last and query:sub(1, last - 1) or "")
      elseif key == "<Space>" then
        filter(query .. " ")
      elseif utf8.len(key) == 1 then
        filter(query .. key)
      end
    end
    render()
  end
end

local function switch(spec, session)
  if maki.session.current() ~= session then
    return
  end
  local state, err = maki.model.set(spec)
  maki.ui.flash(state and ("Selected " .. state.spec) or ("Provider switch failed: " .. err))
end

local function open_provider(opts)
  if picker then
    return
  end
  local session, err = maki.session.current()
  if not session then
    maki.ui.flash(err)
    return
  end
  local arg = opts.args:match("^%s*(.-)%s*$")
  if arg:find("/", 1, true) then
    switch(arg, session)
    return
  end
  local current
  current, err = maki.model.get()
  if not current then
    maki.ui.flash(err)
    return
  end
  local specs
  specs, err = maki.model.available()
  if not specs then
    maki.ui.flash(err)
    return
  end
  local providers, models = {}, {}
  for _, spec in ipairs(specs) do
    local slug = spec:match("^([^/]+)/.+$")
    if slug then
      if not models[slug] then
        providers[#providers + 1] = slug
        models[slug] = {}
      end
      models[slug][spec] = true
    end
  end
  table.sort(providers)
  if #providers == 0 then
    maki.ui.flash(EMPTY_MODELS)
    return
  end
  if maki.session.current() ~= session then
    return
  end
  local slug = arg ~= "" and arg or choose(providers, " Providers ", current.spec:match("^[^/]+"), session)
  if not slug then
    return
  end
  if not models[slug] then
    maki.ui.flash(
      "No models listed for '" .. slug .. "'. Run maki auth login " .. slug .. ", then open /model to refresh."
    )
    return
  end
  local choices = {}
  for spec in pairs(models[slug]) do
    choices[#choices + 1] = spec
  end
  table.sort(choices)
  if maki.session.current() ~= session then
    return
  end
  local spec = choose(choices, " " .. slug .. " models ", current.spec, session)
  if spec then
    switch(spec, session)
  end
end

maki.api.register_command({
  name = "/provider",
  description = "Switch between saved providers and their models",
  nargs = "?",
  handler = open_provider,
})

maki.api.create_autocmd("SessionFocusChanged", { callback = close_picker })
maki.api.create_autocmd("SessionEnd", {
  callback = function(ev)
    if picker and ev.data and picker.session == ev.data.session_id then
      close_picker()
    end
  end,
})
