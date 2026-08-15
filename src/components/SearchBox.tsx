import { useState } from "react";

import { SearchIcon } from "./icons";

// Visual entry point only. The full search interface (result list, invocation
// of the `search` command) is a later Phase 10 task. The input is a real,
// accessible control so keyboard users can interact with it.
export default function SearchBox() {
  const [value, setValue] = useState("");

  return (
    <div className="nex-search">
      <label htmlFor="nex-search-input" className="nex-sr-only">
        Search conversations
      </label>
      <SearchIcon className="nex-search-icon" />
      <input
        id="nex-search-input"
        type="search"
        className="nex-search-input"
        placeholder="Search conversations"
        value={value}
        onChange={(event) => setValue(event.target.value)}
        autoComplete="off"
        spellCheck={false}
      />
    </div>
  );
}
