<script setup lang="ts">
import { UserDashboardView } from 'floppa-web-shared/views'
import VpnCard from '../components/VpnCard.vue'
</script>

<template>
  <UserDashboardView skip-loading-spinner>
    <template #vpn-widget>
      <VpnCard />
    </template>
  </UserDashboardView>
</template>
