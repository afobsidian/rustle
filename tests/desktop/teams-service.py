"""Isolated upstream D-Bus protocol simulator; never used as live Teams proof."""
import json
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

BUS = "com.github.IsmaelMartinez.teams_for_linux"
PATH = "/com/github/IsmaelMartinez/teams_for_linux"
CONTROL = BUS + ".Control"
xml = f"""<node>
<interface name="{CONTROL}">
<method name="GetState"><arg type="s" direction="out"/></method>
<signal name="InCallChanged"><arg type="b"/></signal>
</interface>
<interface name="org.rustle.Test">
<method name="SetInCall"><arg type="b" direction="in"/></method>
<method name="Malformed"><arg type="b" direction="in"/></method>
<method name="GetCount"><arg type="u" direction="out"/></method>
<method name="Quit"/>
</interface></node>"""
loop = GLib.MainLoop()
connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
active = False
malformed = False
count = 0


def method(connection, sender, path, interface, member, parameters, invocation):
    global active, malformed, count
    if member == "GetState":
        count += 1
        state = "broken" if malformed else json.dumps({"inCall": active, "microphoneState": "muted"})
        invocation.return_value(GLib.Variant("(s)", (state,)))
    elif member == "GetCount":
        invocation.return_value(GLib.Variant("(u)", (count,)))
    else:
        if member == "SetInCall":
            active = parameters.unpack()[0]
            connection.emit_signal(None, PATH, CONTROL, "InCallChanged", GLib.Variant("(b)", (active,)))
        elif member == "Malformed":
            malformed = parameters.unpack()[0]
        elif member == "Quit":
            GLib.idle_add(loop.quit)
        invocation.return_value(GLib.Variant("()", ()))


node = Gio.DBusNodeInfo.new_for_xml(xml)
for interface in node.interfaces:
    connection.register_object(PATH, interface, method, None, None)
owner = Gio.bus_own_name_on_connection(connection, BUS, Gio.BusNameOwnerFlags.NONE, None, None)
loop.run()
