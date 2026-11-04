# What a device on the network actually sees of a running SillHost: a separate Bonjour client looks
# for _sill._tcp and checks the record the iPad reads. CI runs it beside the real program.
import sys, time
from zeroconf import ServiceBrowser, ServiceListener, Zeroconf

found = {}
class Listener(ServiceListener):
    def add_service(self, zc, type_, name):
        info = zc.get_service_info(type_, name, 3000)
        if info: found[name] = info
    def update_service(self, zc, type_, name): self.add_service(zc, type_, name)
    def remove_service(self, zc, type_, name): pass

zc = Zeroconf()
ServiceBrowser(zc, "_sill._tcp.local.", Listener())
end = time.time() + 12
while time.time() < end and not found:
    time.sleep(0.2)
zc.close()
if not found:
    print("Bonjour: nothing found for _sill._tcp"); sys.exit(1)
for name, i in found.items():
    p = {k.decode(): (v.decode() if v else v) for k, v in i.properties.items()}
    print(f"Bonjour: {name} port {i.port} host {i.server} addresses {i.parsed_addresses()} txt p={p.get('p')} r={len(p.get('r') or '')} characters")
    ok = p.get("p") == "1" and len(p.get("r") or "") == 16 and i.port == 17999 and i.parsed_addresses()
    sys.exit(0 if ok else 1)
