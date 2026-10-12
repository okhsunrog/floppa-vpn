<script setup lang="ts">
defineProps<{ name: string; avatar?: string }>()
</script>

<template>
  <span class="inline-flex items-center gap-2 min-w-0 max-w-full">
    <UAvatar :src="avatar" :alt="name" icon="i-lucide-user" size="2xs" class="shrink-0" />
    <span class="min-w-0 break-words">{{ name }}</span>
  </span>
</template>
