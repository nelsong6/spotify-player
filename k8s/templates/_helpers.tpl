{{/* The security every container here runs with. */}}
{{- define "spt-session.containerSecurity" -}}
allowPrivilegeEscalation: false
capabilities:
  drop: [ALL]
{{- end }}
