#!/usr/bin/env python3
# Minimal real StatusNotifierItem for manually testing the statustray plugin (D2a/D2b).
# Registers a KDE-style SNI item (IconName=firefox, Active) with a DBusMenu context menu
# (Open / separator / Quit / a submenu entry to exercise the flatten path).
# Usage: run wafflebar (becomes the Watcher), then: python3 tests/fixtures/sni_item.py
# Requires python3-gi (Gio). Not part of CI — a debugging aid for tray regressions.
import os, gi
gi.require_version('Gio','2.0')
from gi.repository import Gio, GLib
V = GLib.Variant

SNI_XML = '''<node><interface name="org.kde.StatusNotifierItem">
 <property name="Category" type="s" access="read"/><property name="Id" type="s" access="read"/>
 <property name="Title" type="s" access="read"/><property name="Status" type="s" access="read"/>
 <property name="IconName" type="s" access="read"/><property name="ItemIsMenu" type="b" access="read"/>
 <property name="Menu" type="o" access="read"/>
 <method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
</interface></node>'''
MENU_XML = '''<node><interface name="com.canonical.dbusmenu">
 <method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/>
   <arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method>
 <method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/></method>
 <method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
 <signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>
</interface></node>'''
SNI = {'Category':'ApplicationStatus','Id':'wafflebar-menu-test','Title':'Menu Test','Status':'Active','IconName':'firefox','ItemIsMenu':False,'Menu':'/MenuBar'}

def vnode(nid, props):
    p = {k: (V('s',v) if isinstance(v,str) else V('b',v)) for k,v in props.items()}
    return V('v', V('(ia{sv}av)', (nid, p, [])))  # wrapped for embedding in an `av`

def sni_meth(c,s,p,i,m,params,inv):
    if m=='Activate': print('ACTIVATED',flush=True); inv.return_value(None)
def sni_get(c,s,p,i,prop):
    v=SNI[prop]
    if prop=='Menu': return V('o',v)
    return V('b',v) if isinstance(v,bool) else V('s',v)
def menu_meth(c,s,p,i,m,params,inv):
    if m=='GetLayout':
        children = [
            vnode(1, {'label':'Open','type':'standard','enabled':True,'visible':True}),
            vnode(2, {'type':'separator'}),
            vnode(3, {'label':'Quit','enabled':True,'visible':True}),
            vnode(4, {'label':'More','children-display':'submenu'}),  # triggers flatten-warn
        ]
        root = (0, {}, children)   # raw tuple for (ia{sv}av)
        inv.return_value(V('(u(ia{sv}av))', (1, root)))
    elif m=='Event':
        print('MENU CLICK', params.get_child_value(0).get_int32(), flush=True); inv.return_value(None)
    elif m=='AboutToShow':
        inv.return_value(V('(b)',(False,)))

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
bus.register_object('/StatusNotifierItem', Gio.DBusNodeInfo.new_for_xml(SNI_XML).interfaces[0], sni_meth, sni_get, None)
bus.register_object('/MenuBar', Gio.DBusNodeInfo.new_for_xml(MENU_XML).interfaces[0], menu_meth, None, None)
name='org.kde.StatusNotifierItem-%d-1'%os.getpid()
Gio.bus_own_name_on_connection(bus,name,Gio.BusNameOwnerFlags.NONE,None,None)
bus.call_sync('org.kde.StatusNotifierWatcher','/StatusNotifierWatcher','org.kde.StatusNotifierWatcher','RegisterStatusNotifierItem',V('(s)',(name,)),None,Gio.DBusCallFlags.NONE,2000,None)
print('registered '+name,flush=True)
GLib.MainLoop().run()
