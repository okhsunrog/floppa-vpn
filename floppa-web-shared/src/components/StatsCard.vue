<script setup lang="ts">
defineProps<{
  label: string
  value: string | number
  icon?: string
  large?: boolean
}>()
</script>

<template>
  <UCard class="min-w-[120px]">
    <div class="flex flex-col items-center text-center">
      <UIcon v-if="icon" :name="icon" class="text-2xl mb-2 text-[var(--ui-primary)]" />
      <div :class="['font-semibold', large ? 'text-3xl' : 'text-2xl']">
        {{ value }}
      </div>
      <div class="text-sm text-[var(--ui-text-muted)] mt-1">{{ label }}</div>
    </div>
  </UCard>
</template>
