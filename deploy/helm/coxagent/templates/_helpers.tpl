{{- define "coxagent.labels" -}}
app.kubernetes.io/name: coxagent
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}
{{- define "coxagent.envFromSecret" -}}
- name: COXAGENT_DB_DSN
  valueFrom: { secretKeyRef: { name: {{ .Values.backing.existingSecret }}, key: db-dsn } }
- name: COXAGENT_AUTH_DSN
  valueFrom: { secretKeyRef: { name: {{ .Values.backing.existingSecret }}, key: auth-dsn, optional: true } }
- name: COXAGENT_REDIS_URL
  valueFrom: { secretKeyRef: { name: {{ .Values.backing.existingSecret }}, key: redis-url } }
- name: COXAGENT_S3_ACCESS_KEY
  valueFrom: { secretKeyRef: { name: {{ .Values.backing.existingSecret }}, key: s3-access-key, optional: true } }
- name: COXAGENT_S3_SECRET_KEY
  valueFrom: { secretKeyRef: { name: {{ .Values.backing.existingSecret }}, key: s3-secret-key, optional: true } }
{{- end -}}
