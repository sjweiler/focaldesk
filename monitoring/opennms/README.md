# OpenNMS Horizon 36 collection for FocalDesk

Render an importable [collection source](focaldesk.xml.in) using the
[Net-SNMP private experimentation subtree](https://www.net-snmp.org/wiki/index.php/Writing_your_own_MIBs)
for the disposable loopback lab:

```sh
lab_oid=.1.3.6.1.4.1.8072.9999.9999.42
python3 monitoring/opennms/render-profile.py "$lab_oid" > focaldesk-lab.xml
FOCALDESK_SNMP_LAB_HOLD=1 bash monitoring/mibs/test-agentx-lab.sh "$lab_oid"
```

The lab exposes its configured `sysObjectID.0` to the SNMPv3 user and prints
the path to the private client configuration after its checks pass. Keep it
running while testing OpenNMS. The OpenNMS polling process must share the
lab's network namespace to reach its loopback address.

1. In **Administration → Manage SNMP Data Collection Config**, import
   `focaldesk-lab.xml` into the profile assigned to the test node's SNMP
   collection. The exact `sysObjectID.0` match limits the source to the lab
   agent identity. The [XML import API](https://docs.opennms.com/horizon/36/development/rest/snmp-datacollection-config-rest-api.html)
   also accepts the source file and an existing profile name.
2. Configure SNMPv3 for the lab node using port `11661`, SHA-256 and AES
   with security level `authPriv`. The username and generated passphrases
   are in the private client config printed by the lab. See the
   [SNMP configuration reference](https://docs.opennms.com/horizon/36/development/rest/snmp_configuration.html).
3. Provision a test node with `127.0.0.1` as its primary SNMP interface. Run
   the [ad hoc collector](https://docs.opennms.com/horizon/36/operation/deep-dive/performance-data-collection/shell/adhoc-collection.html)
   from the Karaf shell, replacing the requisition, foreign ID and profile
   with the values configured in OpenNMS:

   ```text
   opennms:collect -n FOCAL-LAB:focaldesk-lab org.opennms.netmgt.collectd.SnmpCollector 127.0.0.1 collection=default
   ```

Expect compositor uptime and epochs at node level and separate
`focalDeskGpuIndex` and `focalDeskDisplayIndex` resources. The profile
collects recovery and presentation counters as Counter64, along with row
state, freshness, and mode gauges. A sample status of 4 means stale. For
rate analysis, discard deltas across producer-epoch changes or stale samples.
If the inventory epoch changes, rediscover the rows before interpreting the
stored index history. The profile supplies those attributes for inspection.
It does not configure thresholding, automatic series resets, or recovery events.

## Production profile

Import [focaldesk-production.xml](focaldesk-production.xml) into the production
collection profile. It uses `.1.3.6.1.4.1.67726.1.1` for both the collected
object subtree and the exact `sysObjectID.0` match. Set the master and adapter
to the matching OIDs. If the master advertises another Focal sysObjectID,
render a profile with `--sys-object-id`. Keep the lab source in a separate
profile or replace it during migration, and keep lab collection history
separate from production history.

## Rootless Podman bridge

For a bridged OpenNMS container, discover its gateway and start the lab in
the Podman network namespace. Run this on the host from the repository root:

```sh
podman inspect opennms-horizon --format '{{range $name,$net := .NetworkSettings.Networks}}{{$name}} {{$net.Gateway}}{{println}}{{end}}'
lab_oid=.1.3.6.1.4.1.8072.9999.9999.42
FOCALDESK_SNMP_LAB_ADDRESS=10.89.1.1 FOCALDESK_SNMP_LAB_HOLD=1 \
  podman unshare --rootless-netns bash monitoring/mibs/test-agentx-lab.sh \
  "$lab_oid" "$XDG_RUNTIME_DIR/focaldesk-vulkan-metrics.json"
```

Use the actual gateway from the inspection in the command. Set the test
node's SNMP address and the ad hoc collector host to that gateway, with
UDP port `11661`. In the example, the gateway is `10.89.1.1` on
`opennms-net`. The lab binds only that bridge address inside the rootless
network namespace and uses the same temporary SNMPv3 credentials.

## Resource graphs

Install [focaldesk-graph.properties](focaldesk-graph.properties) in
`${OPENNMS_HOME}/etc/snmp-graph.properties.d/`. On the FocalDesk node's
**Resource Graphs** page, select a GPU or display resource to find its graphs.

To enable the built-in SNMP response-time graph, add the response repository
and base name parameters to the active SNMP service's poller package. Use
the same response repository as the package's ICMP monitor:

```xml
<parameter key="rrd-repository" value="/opt/opennms/share/rrd/response" />
<parameter key="rrd-base-name" value="snmp" />
<parameter key="ds-name" value="snmp" />
```

Reload the poller configuration or restart OpenNMS. On the node's resource
graphs, use the interface response and service status resources respectively.
Response time history begins with the first successful poll after enabling
storage. Select a time range that includes recent polls.

Optional NVIDIA busy and VRAM graphs appear for GPU rows with supported
sensor values. Import the updated production collection source to collect
the three additional columns, and enable the AgentX NVIDIA sampler.

The FocalDesk Input Dispatch Delay graph uses the increase in accumulated
delay divided by the increase in event count to report milliseconds per event
for keyboard and mouse motion. Intervals without events have no line. Changes
in the compositor epoch mark counter discontinuities. Re-import the production
collection source when adding the four input counter columns to an existing
OpenNMS installation.

## GPU recovery failure event

Upload [focaldesk-events.xml](focaldesk-events.xml) through the OpenNMS
[Event Configuration](https://docs.opennms.com/horizon/36/operation/deep-dive/events/event-configuration.html)
page. Add a dedicated SNMPv3 receiver user with security level authPriv,
SHA-256, and AES in **Integrations → Trap Configuration**, then configure
the Net-SNMP master to send INFORMs to that listener. The monitoring
[notification instructions](../mibs/README.md#gpu-recovery-failure-inform)
describe how to enable the adapter and run its isolated test.

Look for `uei.opennms.org/vendor/FocalDesk/traps/gpuRecoveryFailed` in
**Events**. The existing GPU threshold remains responsible for the alarm
lifecycle, including clearing the alarm when a fresh healthy sample arrives.

Re-import the same event file to enable the compositor snapshot events.
`compositorMetricsStale` reports a 30-second interruption, and
`compositorMetricsResumed` carries a restart flag derived from the producer
epoch. The same authPriv receiver can process all three FocalDesk INFORMs.

## Reproduce the installed OpenNMS setup

1. Install the [persistent SNMP user service](../systemd/README.md).
2. Import the [production collection source](focaldesk-production.xml) into
   the node's collection profile, and [the event definitions](focaldesk-events.xml)
   into Event Configuration. Set the polling interface and SNMPv3 credentials.
   For notifications, also configure the SNMPv3 trap receiver.
3. Install the [graph definitions](focaldesk-graph.properties) and add the
   [Graph Collection fragment](graph-collection.fragment.xml) to
   `ksc-performance-reports.xml`. Use the actual requisition and foreign ID,
   node interface, and GPU and display row indexes.
4. Merge the [threshold group](thresholds.fragment.xml) into `thresholds.xml`
   and the [threshold package](threshd-package.fragment.xml) into
   `threshd-configuration.xml`. If needed, apply the SNMP response-time
   settings in [Resource graphs](#resource-graphs).
5. Reload OpenNMS and verify a fresh collection, the graph panes, and the
   notification receiver. The [isolated INFORM test](../mibs/README.md#gpu-recovery-failure-inform)
   exercises synthetic failures without interrupting the desktop.
