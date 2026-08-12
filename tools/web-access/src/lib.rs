#[path = "web_fetch.rs"]
mod web_fetch_impl;
#[path = "web_search.rs"]
mod web_search_impl;

use web_fetch_impl::web_fetch;
use web_search_impl::web_search;

e_agent_tool::extension!(web_access, [web_search]);
