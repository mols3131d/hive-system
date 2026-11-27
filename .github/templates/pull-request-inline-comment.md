(*GitHub already provides file and line context. One actionable finding per inline comment.*)

{{ finding }}

{% if impact %}
**Impact:** {{ impact }}
{% endif %}

{% if required_change %}
**Required change:** {{ required_change }}
{% endif %}

{% if evidence %}
**Evidence:** {{ evidence }}
{% endif %}
