# Moving the radio's session from the NAS to the home cluster

The steps for [SPT-35](https://openproject.romaine.life/wp/SPT-35), part of
[HOMEINFRA-1](https://openproject.romaine.life/wp/HOMEINFRA-1): spt-session (Snapcast and the
Spotify session) leaves the NAS for the home cluster. The NAS keeps two shares the PCs read:

| Share | Was | Holds | To the cluster |
| --- | --- | --- | --- |
| `spt` | `Files/spt` | the relay key, the saved session, the shared screen, the kept playlists' state, the kept library | NFS, read-write |
| `spt-config` | `tools/spt/config` | `app.toml`, `menu.yaml` | NFS, read-only |

The session's own state (the Spotify sign-in, Snapcast's `server.json`, the handover's slots
and `history.db`) moves to the VM's disk. The PCs reach the session as `spt.home.romaine.life`
on 1704, 1706 and 1707, which k3s's service load balancer puts on 192.168.50.41. Run these
from a home PC's Git Bash with `ssh nas` and `ssh cluster`, in a checkout of this repo. No
command here prints a secret or puts one on a command line.

The radio is silent from step 2.2 until the session starts in 2.5: a few minutes, since the
state is 42 MB.

## Before

These must be live, not only merged. Live since 2026-11-10:

- `spt.home.romaine.life`, a zone on the NAS's DNS Server answering the NAS (192.168.50.42,
  TTL 60), made by hand so the PCs use the new name while the NAS still serves it; homesvc
  owns it once the Service exists.
- This repo's `cast_server` default, `spt.home.romaine.life`, on every PC (v0.25.1-110).
- The shares `spt` and `spt-config`, exported to the cluster (nelsong6/home-cluster
  `bootstrap/nas-exports.sh`), with `spt-config` holding a copy of `tools/spt/config`.
- fzt-automate's `tool-config:spt` reading `spt-config` (v0.1.99).
- OpenBao `secret/services/spt-session` (`release-read-token`, `registry-read-token`: one
  Forgejo token, `spt-session-read`, with `read:repository` and `read:package`),
  `secret/services/spt-session-build` (`registry-token`: `spt-session-build`, `write:package`),
  and the `spotify-player-publish` role (nelsong6/openbao), which binds the subject Forgejo
  sends, `repo:nelsong6/spotify-player:ref:refs/heads/master`.

Still to land before step 2:

- An image pinned in `values.yaml` by CI's pin PR.
- nelsong6/home-cluster's Argo CD app `spt-session`, the `nas-backup` Role in `spt`, and
  homesvc's permission to list Services.
- at-home-tools' home DNS for LoadBalancer Services (`home.romaine.life/hostname`) and
  home-sync reading `spt-config`, installed on the NAS with `homesvc update-health` and on
  every PC.
- nelsong6/nas-backup's `spt` job reading the cluster, ready to merge and deploy in step 2.7.

## 1. Ship it stopped

Merge `k8s/` here, then home-cluster's app. Nothing runs: the session is at 0 replicas, with
no Service. The `state` volume stays `Pending` until a pod uses it.

```bash
ssh cluster sudo k3s kubectl get application spt-session -n argocd
ssh cluster sudo k3s kubectl get pvc,cm,externalsecret,deploy,svc,pods -n spt
```

Both ExternalSecrets `SecretSynced`, the volume `Pending`, no pods.

## 2. Take over

1. The takeover PR, in this repo: set each `# takeover:` line in `k8s/values.yaml` to its
   value, remove CI's `nas-session` job, and rewrite the docs' NAS sections (README's home
   cast, AGENTS.md's fork notes, `docs/design.md`).

   ```bash
   sed -E -i 's/^( *[^:#]+): [^#]*# takeover: (.*)$/\1: \2/' k8s/values.yaml
   ```

   Argo CD syncs it within three minutes: the pod waits in `wait-for-data`, and the Service
   takes the ports. Within a minute homesvc points `spt.home.romaine.life` at 192.168.50.41,
   and the PCs lose the session within the record's minute.

   ```bash
   ssh cluster sudo k3s kubectl get pods,pvc,svc -n spt
   nslookup spt.home.romaine.life
   ```

   The pod `Init:0/2` and the volume `Bound`. A pod stuck in `ContainerCreating` hasn't
   mounted an NFS share (`kubectl describe pod -n spt -l app=spt-session` says why): stop
   here and roll back (section 4).

2. Stop the NAS's session. Nothing writes its folders after this:

   ```bash
   ssh nas 'cd /volume1/docker/spt-session && sudo /usr/local/bin/docker compose stop'
   ```

3. Copy the shared folder into the `spt` share on the NAS itself, give every copy the share's
   own permissions, and point the PCs' settings at it. A copy made as root is a plain Linux
   file that no ACL reaches, which the session, writing over NFS, can't change (on
   2026-11-10 the keeper failed to save until each file got the share's ACL). A PC still
   running reads the relay key from `Files/spt` until it restarts; it's the same key.

   ```bash
   ssh nas 'sudo bash -s' <<'EOF'
   set -euo pipefail
   cp -r --no-preserve=mode,ownership /volume1/Files/spt/. /volume1/spt/
   find /volume1/spt /volume1/spt-config -mindepth 1 -not -path '*@eaDir*' | while read -r p; do
     /usr/syno/bin/synoacltool -copy "/volume1/$(echo "${p#/volume1/}" | cut -d/ -f1)" "$p" >/dev/null
   done
   python3 - /volume1/spt-config/app.toml <<'PY'
   import re, sys
   path = sys.argv[1]
   text = open(path, encoding="utf-8").read()
   text, n = re.subn(r"(?m)^session_file = .*$", lambda _: r"session_file = '\\NelsonNAS\spt\last_session.json'", text)
   assert n == 1, "app.toml has no single session_file line"
   open(path, "w", encoding="utf-8").write(text)
   PY
   grep '^session_file' /volume1/spt-config/app.toml
   for d in /volume1/Files/spt /volume1/spt; do
     find "$d" -type f -printf '%s\n' | awk -v d="$d" '{ n++; s += $1 } END { print d ": " n " files, " s " bytes" }'
   done
   sha256sum /volume1/Files/spt/relay.key /volume1/spt/relay.key
   EOF
   ```

   `session_file = '\\NelsonNAS\spt\last_session.json'`, and the same counts, bytes and key.

4. Copy the session's own state into the `state` volume through a helper pod, as the NAS's
   user and group (1026:100, the files' owner there too):

   ```bash
   ssh cluster 'sudo k3s kubectl apply -f -' <<'EOF'
   apiVersion: v1
   kind: Pod
   metadata:
     name: state-copy
     namespace: spt
   spec:
     securityContext:
       runAsNonRoot: true
       runAsUser: 1026
       runAsGroup: 100
       seccompProfile:
         type: RuntimeDefault
     containers:
       - name: copy
         image: busybox:1.37.0
         command: [sleep, "3600"]
         securityContext:
           allowPrivilegeEscalation: false
           capabilities:
             drop: [ALL]
         volumeMounts:
           - name: state
             mountPath: /state
     volumes:
       - name: state
         persistentVolumeClaim:
           claimName: state
   EOF
   ssh cluster sudo k3s kubectl wait --for=condition=Ready pod/state-copy -n spt --timeout=120s
   ssh nas "sudo tar -C /volume1/docker/spt-session -cf - cache data" |
     ssh cluster "sudo k3s kubectl exec -i -n spt state-copy -- tar -C /state -xf -"
   ssh nas 'sudo sh -s' <<'EOF'
   cd /volume1/docker/spt-session
   find cache data -type f -printf '%s\n' | awk '{ n++; s += $1 } END { print n " files, " s " bytes" }'
   sha256sum data/slots/history.db cache/credentials.json
   EOF
   ssh cluster 'sudo k3s kubectl exec -i -n spt state-copy -- sh -s' <<'EOF'
   cd /state
   find cache data -type f -print0 | xargs -0 -n 500 stat -c %s | awk '{ n++; s += $1 } END { print n " files, " s " bytes" }'
   sha256sum data/slots/history.db cache/credentials.json
   EOF
   ```

   The same counts, bytes and hashes on both sides.

5. Let the session start, and remove the helper. The stager stages the pinned release first:

   ```bash
   ssh cluster sudo k3s kubectl exec -n spt state-copy -- touch /state/.data-ready
   ssh cluster sudo k3s kubectl delete pod state-copy -n spt
   ssh cluster sudo k3s kubectl rollout status deployment/spt-session -n spt --timeout=10m
   ssh cluster sudo k3s kubectl logs -n spt deploy/spt-session -c stager
   ```

   `stager: staged <tag>` and `stager: the session's slots start on <tag>`, with `<tag>` the
   one `values.yaml` pins.

6. Every PC plays it (section 3).

7. Merge and deploy nas-backup's `spt` job, which reads `history.db` from the pod.

## 3. Verify

- `nslookup spt.home.romaine.life` answers 192.168.50.41 on every PC.
- Snapcast's client list has every PC with spt open, connected:

  ```bash
  ssh cluster sudo k3s kubectl exec -n spt deploy/spt-session -c snapserver -- \
    jq -r '.Groups[].clients[] | "\(.id) connected=\(.connected) muted=\(.config.volume.muted)"' /state/data/server.json
  ```

- The radio plays aloud on each PC, the shared screen follows, and the palette's library
  loads (the relay).
- The next release CI makes reaches the pod by its pin PR with no restart: the stager logs
  `restarted the waiting slot`, and the new release takes the session over at the next song
  change (`log-search --pc cluster --app spt-session handover`).

## 4. Roll back

Until the teardown, the NAS still has everything as it was at step 2.2.

1. Revert the takeover PR (`serve: false`); Argo CD stops the session and removes the Service.
2. Point `spt.home.romaine.life` back at the NAS (192.168.50.42) on the NAS's DNS Server, since
   homesvc never removes a record.
3. Copy back what the cluster's session wrote since the takeover: `history.db` from the
   `state` volume, then start the NAS's session:

   ```bash
   ssh nas 'cd /volume1/docker/spt-session && sudo /usr/local/bin/docker compose start'
   ```

The `spt` share's files stay valid for the NAS's session, which reads `Files/spt` and
`tools/spt/config`: copy `spt` back over `Files/spt` if the cluster ran long enough to change it.

## 5. Teardown, a few days on

- The NAS's container and image (`docker compose down --rmi all` in
  `/volume1/docker/spt-session`), then `/volume1/docker/spt-session`.
- `/volume1/tools/spt` (every staged release and the old `config/`) and `/volume1/Files/spt`.
- This repo's `nas/`, and the docs' last mentions of it.
- `homesvc spt-upgrade` (at-home-tools), its restricted key in the NAS account's
  `authorized_keys`, and the runner's `spt_id_ed25519` (OpenBao `secret/services/forgejo`).
