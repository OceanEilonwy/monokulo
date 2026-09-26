# Decisions: Monokulo–engine boundary work pack

Every place the plan in `README.md` left a detail open, or where the work had to deviate. Numbered; newest last.

Format for each entry:
- **Step:**
- **Decision:**
- **Alternatives considered:**
- **Why:**

### 1. Public bind is warned about, not refused
- **Step:** 1
- **Decision:** A non-private `server.bind` prints a `WARNING` to stderr at boot (after binding, using the listener's real local address) but still starts. Private means loopback, RFC 1918, IPv6 ULA `fc00::/7`, or link-local (`169.254/16`, `fe80::/10`); IPv4-mapped IPv6 is judged by its IPv4 part. `0.0.0.0`/`::` and CGNAT `100.64/10` count as public.
- **Alternatives considered:** Refuse to start on a public bind unless an override setting is set; also classify CGNAT as private.
- **Why:** The plan asks for a loud warning, not a refusal, and some operators run the engine on a separate host in a private network reachable only by monokulo. Classifying from the listener's local address (not the setting string) also handles hostnames like `localhost:8443`. CGNAT space is shared with other ISP customers, so it is not private to the operator.
