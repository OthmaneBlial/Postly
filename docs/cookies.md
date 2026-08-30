# Local cookies

Postly keeps HTTP cookies in a session jar shared by cloned engine handles.
Responses expose parsed `Set-Cookie` metadata, and the native workspace lets
you add explicit request cookies from the Cookies tab.

Saved-request workflows opt into `.postly/cookies.json`. The file is ignored
by Git, stays inside the workspace, is capped at 1 MiB, and is loaded only by
that workspace's HTTP engine. Unsaved CLI requests keep cookies in memory and
do not create a cookie file.

Cookie values are credentials. Do not commit `.postly/cookies.json`, paste it
into issue reports, or share it with an exported collection. The local cookie
file is convenience persistence, not OS-keychain protection; keychain-backed
secret storage remains a separate roadmap item.
